/* Copyright (c) 2025 OceanBase. Licensed under the Apache License, Version 2.0. */
#pragma once

#include <algorithm>
#include <cerrno>
#include <charconv>
#include <chrono>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <glob.h>
#include <limits>
#include <map>
#include <memory>
#include <set>
#include <stdexcept>
#include <string>
#include <sys/stat.h>
#include <unistd.h>
#include <fcntl.h>
#include <vector>
#include <rapidjson/document.h>
#include "sql/engine/basic/file_scan_types.h"
#include "sql/engine/basic/file_scan_parquet.h"

namespace oceanbase { namespace sql { namespace file_sql {

inline bool valid_utf8(const std::string &value)
{
  for (size_t i = 0; i < value.size();) {
    unsigned char first = static_cast<unsigned char>(value[i++]);
    if (first < 0x80) continue;
    unsigned count = first >= 0xc2 && first <= 0xdf ? 1
                   : first >= 0xe0 && first <= 0xef ? 2
                   : first >= 0xf0 && first <= 0xf4 ? 3 : 0;
    if (!count || i + count > value.size()) return false;
    unsigned code = first & ((1u << (6 - count)) - 1);
    for (unsigned j = 0; j < count; ++j) {
      unsigned char c = static_cast<unsigned char>(value[i++]);
      if ((c & 0xc0) != 0x80) return false;
      code = (code << 6) | (c & 0x3f);
    }
    if ((count == 1 && code < 0x80) || (count == 2 && code < 0x800)
        || (count == 3 && code < 0x10000) || code > 0x10ffff
        || (code >= 0xd800 && code <= 0xdfff)) return false;
  }
  return true;
}

inline bool valid_date(const std::string &s)
{
  if (s.size() < 10 || s[4] != '-' || s[7] != '-') return false;
  for (size_t i = 0; i < 10; ++i) if (i != 4 && i != 7 && (s[i] < '0' || s[i] > '9')) return false;
  const int year = std::atoi(s.substr(0, 4).c_str());
  const unsigned month = std::atoi(s.substr(5, 2).c_str());
  const unsigned day = std::atoi(s.substr(8, 2).c_str());
  return year > 0 && std::chrono::year_month_day{std::chrono::year{year},
      std::chrono::month{month}, std::chrono::day{day}}.ok();
}

inline Kind classify(const std::string &text)
{
  if (text == "true" || text == "false" || text == "TRUE" || text == "FALSE") return Kind::Bool;
  int64_t integer;
  const auto parsed = std::from_chars(text.data(), text.data() + text.size(), integer);
  if (!text.empty() && parsed.ec == std::errc() && parsed.ptr == text.data() + text.size()) return Kind::Int;
  // An out-of-range integer must not silently become a rounded floating value.
  if (parsed.ec != std::errc::result_out_of_range
      && text.find_first_of(".eE") != std::string::npos && !text.empty()) {
    char *end = nullptr;
    errno = 0;
    const double number = std::strtod(text.c_str(), &end);
    if (end == text.c_str() + text.size() && errno != ERANGE && std::isfinite(number)) return Kind::Double;
  }
  if (valid_date(text)) {
    if (text.size() == 10) return Kind::Date;
    if (text.size() >= 19 && (text[10] == ' ' || text[10] == 'T')
        && text[13] == ':' && text[16] == ':') {
      bool digits = true;
      for (size_t i : {11u, 12u, 14u, 15u, 17u, 18u}) digits &= text[i] >= '0' && text[i] <= '9';
      bool fraction = text.size() == 19 || (text.size() > 20 && text.size() <= 26 && text[19] == '.');
      for (size_t i = 20; i < text.size(); ++i) fraction &= text[i] >= '0' && text[i] <= '9';
      if (digits && fraction && std::atoi(text.substr(11, 2).c_str()) <= 23
          && std::atoi(text.substr(14, 2).c_str()) <= 59
          && std::atoi(text.substr(17, 2).c_str()) <= 59) return Kind::DateTime;
    }
  }
  return Kind::String;
}

inline Kind widen(Kind a, Kind b)
{
  if (a == b || b == Kind::Null) return a;
  if (a == Kind::Null) return b;
  if ((a == Kind::Int && b == Kind::Double) || (a == Kind::Double && b == Kind::Int)) return Kind::Double;
  if ((a == Kind::Date && b == Kind::DateTime) || (a == Kind::DateTime && b == Kind::Date)) return Kind::DateTime;
  return Kind::String;
}

inline std::string canonical(const std::string &path)
{
  char *resolved = ::realpath(path.c_str(), nullptr);
  if (!resolved) throw std::runtime_error("file SQL: cannot resolve file path");
  std::string result(resolved);
  std::free(resolved);
  return result;
}

inline std::string checked_path(const std::string &path, const std::string &secure_root)
{
  if (secure_root.empty() || secure_root == "NULL" || secure_root == "null")
    throw std::runtime_error("file SQL: secure_file_priv disallows file access");
  const std::string root = canonical(secure_root);
  const std::string result = canonical(path);
  if (root != "/" && result != root && result.compare(0, root.size() + 1, root + "/") != 0)
    throw std::runtime_error("file SQL: path is outside secure_file_priv");
  return result;
}

// The reader owns a descriptor, not the pathname: a path replacement cannot redirect
// a scan after validation. Memory is bounded by the schema and one logical record.
class Reader {
public:
  static constexpr size_t MAX_COLUMNS = 4096;
  static constexpr size_t MAX_RECORD = 64 * 1024 * 1024;
  explicit Reader(Options options, std::string secure_root)
      : options_(std::move(options)), secure_root_(std::move(secure_root)) {}
  ~Reader() { if (file_) std::fclose(file_); }
  Reader(const Reader &) = delete;
  Reader &operator=(const Reader &) = delete;

  const std::vector<Column> &columns() const { return columns_; }
  const std::string &path() const { return path_; }
  const std::string &error() const { return error_; }
  const struct stat &identity() const { return identity_; }
  void projection(const std::vector<size_t> &columns) { if(parquet_)parquet_->projection(columns); }

  void open()
  {
    if (file_) throw std::runtime_error("file SQL: reader is already open");
    if (options_.format == "glob") {
      const auto wildcard = options_.path.find_first_of("*?[");
      const auto slash = options_.path.rfind('/', wildcard);
      checked_path(slash == std::string::npos ? "." : options_.path.substr(0, slash + 1), secure_root_);
      glob_t matches {};
      const int status = ::glob(options_.path.c_str(), GLOB_NOSORT, nullptr, &matches);
      if (status != 0 && status != GLOB_NOMATCH) { ::globfree(&matches); fail("file discovery failed"); }
      try {
        if (matches.gl_pathc > 100000) fail("too many matching paths");
        for (size_t i = 0; i < matches.gl_pathc; ++i) {
          std::string path = checked_path(matches.gl_pathv[i], secure_root_);
          struct stat info;
          if (::stat(path.c_str(), &info)) fail("matching file disappeared");
          if (S_ISREG(info.st_mode)) glob_paths_.push_back(path);
        }
      } catch (...) { ::globfree(&matches); throw; }
      ::globfree(&matches);
      std::sort(glob_paths_.begin(), glob_paths_.end());
      columns_.push_back({"file", Kind::String});
      return;
    }
    path_ = checked_path(options_.path, secure_root_);
    const int fd = ::open(path_.c_str(), O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (fd < 0) throw std::runtime_error("file SQL: cannot open input file");
    if (::fstat(fd, &identity_) || !S_ISREG(identity_.st_mode)) {
      ::close(fd); throw std::runtime_error("file SQL: input must be a regular file");
    }
    if (canonical("/proc/self/fd/" + std::to_string(fd)) != path_) {
      ::close(fd); throw std::runtime_error("file SQL: input path changed while opening");
    }
    file_ = ::fdopen(fd, "rb");
    if (!file_) { ::close(fd); throw std::runtime_error("file SQL: cannot create input stream"); }
    if (options_.format == "parquet") {
      parquet_.reset(new parquet::Reader(file_,identity_.st_size));
      columns_=parquet_->columns();
      std::set<std::string> names;
      for(const auto &c:columns_)if(c.name.empty()||!valid_utf8(c.name)||!names.insert(c.name).second)fail("invalid or duplicate Parquet column name");
      verify();return;
    }
    if (options_.format != "csv" && options_.format != "jsonl")
      throw std::runtime_error("file SQL: unsupported file format");
    std::vector<Value> row;
    if (options_.format == "csv" && options_.header && read_csv(row)) {
      std::map<std::string, unsigned> used;
      for (auto &value : row) {
        std::string name = value.text.empty() ? "column" + std::to_string(columns_.size()) : value.text;
        const std::string base = name;
        for (unsigned suffix = 2; used.count(name); ++suffix) name = base + "__" + std::to_string(suffix);
        used[name] = 1;
        columns_.push_back({name, Kind::Null});
      }
    }
    while (next_raw(row, true)) {
      if (options_.format == "csv" && columns_.empty())
        for (size_t i = 0; i < row.size(); ++i) columns_.push_back({"column" + std::to_string(i), Kind::Null});
      if (row.size() != columns_.size()) fail("record has an inconsistent field count");
      for (size_t i = 0; i < row.size(); ++i) {
        columns_[i].kind = widen(columns_[i].kind, row[i].kind);
        columns_[i].nullable = columns_[i].nullable || row[i].kind==Kind::Null;
        if(row[i].kind==Kind::DateTime){const auto point=row[i].text.find('.');if(point!=std::string::npos)columns_[i].scale=std::max(columns_[i].scale,static_cast<int>(row[i].text.size()-point-1));}
      }
    }
    verify();
    if (columns_.empty()) fail("input contains no columns");
    rewind();
  }

  void rewind()
  {
    if (options_.format == "glob") { glob_index_ = 0; return; }
    verify();
    if (parquet_) { parquet_->rewind();return; }
    if (::fseeko(file_, 0, SEEK_SET)) fail("cannot rewind file");
    record_number_ = 0;
    if (options_.format == "csv" && options_.header) { std::vector<Value> ignored; read_csv(ignored); }
  }

  bool next(std::vector<Value> &row)
  {
    if (options_.format == "glob") {
      row.clear();
      if (glob_index_ == glob_paths_.size()) return false;
      row.push_back({glob_paths_[glob_index_++], Kind::String});
      return true;
    }
    verify();
    if (parquet_) {
      const bool available=parquet_->next(row);
      for(const auto &value:row)if(value.kind==Kind::String&&!valid_utf8(value.text))fail("invalid UTF-8 field");
      return available;
    }
    const bool available = next_raw(row, false);
    if (available && row.size() != columns_.size()) fail("record has an inconsistent field count");
    return available;
  }

  void verify() const
  {
    struct stat current, named;
    if (!file_ || ::fstat(::fileno(file_), &current) || ::stat(path_.c_str(), &named)
        || current.st_dev != identity_.st_dev || current.st_ino != identity_.st_ino
        || named.st_dev != identity_.st_dev || named.st_ino != identity_.st_ino
        || current.st_size != identity_.st_size
        || current.st_mtim.tv_sec != identity_.st_mtim.tv_sec
        || current.st_mtim.tv_nsec != identity_.st_mtim.tv_nsec
        || current.st_ctim.tv_sec != identity_.st_ctim.tv_sec
        || current.st_ctim.tv_nsec != identity_.st_ctim.tv_nsec)
      throw std::runtime_error("file SQL: input changed during scan");
  }

private:
  [[noreturn]] void fail(const std::string &message)
  {
    error_ = "file SQL: record " + std::to_string(record_number_ + 1) + ": " + message;
    throw std::runtime_error(error_);
  }

  bool next_raw(std::vector<Value> &row, bool infer)
  {
    return options_.format == "csv" ? read_csv(row) : read_jsonl(row, infer);
  }

  bool read_csv(std::vector<Value> &row)
  {
    row.clear();
    std::string field;
    bool quoted = false, in_quotes = false, closed = false, any = false;
    size_t bytes = 0;
    const auto add_field = [&]() {
      if (row.size() >= MAX_COLUMNS) fail("too many columns");
      if (!valid_utf8(field)) fail("invalid UTF-8 field");
      Kind kind = !quoted && field == options_.nullstr ? Kind::Null : classify(field);
      row.push_back({std::move(field), kind}); field.clear();
      quoted = in_quotes = closed = false;
    };
    for (;;) {
      const int ch = std::fgetc(file_);
      if (ch == EOF) {
        if (std::ferror(file_)) fail("input read error");
        if (in_quotes) fail("unterminated quoted field");
        if (!any) return false;
        add_field(); ++record_number_; return true;
      }
      any = true;
      if (++bytes > MAX_RECORD) fail("record exceeds the size limit");
      const char c = static_cast<char>(ch);
      if (in_quotes) {
        if (c == options_.quote) {
          const int lookahead = std::fgetc(file_);
          if (lookahead == options_.quote && options_.escape == options_.quote) field.push_back(c);
          else { in_quotes = false; closed = true; if (lookahead != EOF) std::ungetc(lookahead, file_); }
        } else if (c == options_.escape && options_.escape != options_.quote) {
          const int escaped = std::fgetc(file_);
          if (escaped == EOF) fail("unterminated escape");
          field.push_back(static_cast<char>(escaped));
        } else field.push_back(c);
      } else if (c == options_.delimiter) {
        add_field();
      } else if (c == '\n' || c == '\r') {
        if (c == '\r') { const int n = std::fgetc(file_); if (n != '\n' && n != EOF) std::ungetc(n, file_); }
        add_field(); ++record_number_; return true;
      } else if (!closed && field.empty() && c == options_.quote) {
        quoted = in_quotes = true;
      } else if (closed) fail("unexpected character after closing quote");
      else field.push_back(c);
    }
  }

  bool read_jsonl(std::vector<Value> &row, bool infer)
  {
    std::string line;
    for (;;) {
      int ch;
      while ((ch = std::fgetc(file_)) != EOF && ch != '\n') {
        if (line.size() >= MAX_RECORD) fail("record exceeds the size limit");
        line.push_back(static_cast<char>(ch));
      }
      if (std::ferror(file_)) fail("input read error");
      if (line.empty() && ch == EOF) return false;
      if (line.find_first_not_of(" \r\t") == std::string::npos) { line.clear(); if (ch == EOF) return false; continue; }
      break;
    }
    if (!valid_utf8(line)) fail("invalid UTF-8 record");
    rapidjson::Document document;
    document.Parse<rapidjson::kParseValidateEncodingFlag>(line.data(), line.size());
    if (document.HasParseError() || !document.IsObject()) fail("expected a flat JSON object");
    std::map<std::string, Value> fields;
    for (auto it = document.MemberBegin(); it != document.MemberEnd(); ++it) {
      std::string name(it->name.GetString(), it->name.GetStringLength());
      Value value;
      const auto &v = it->value;
      if (v.IsNull()) {}
      else if (v.IsString()) { value.text.assign(v.GetString(), v.GetStringLength()); value.kind = Kind::String; }
      else if (v.IsBool()) { value.text = v.GetBool() ? "true" : "false"; value.kind = Kind::Bool; }
      else if (v.IsInt64()) { value.text = std::to_string(v.GetInt64()); value.kind = Kind::Int; }
      else if (v.IsUint64()) { value.text = std::to_string(v.GetUint64()); value.kind = Kind::String; }
      else if (v.IsDouble() && std::isfinite(v.GetDouble())) {
        char buffer[64]; std::snprintf(buffer, sizeof(buffer), "%.17g", v.GetDouble());
        value.text = buffer; value.kind = Kind::Double;
      } else fail("nested objects and arrays are unsupported");
      if (!fields.emplace(name, std::move(value)).second) fail("duplicate JSON field name");
      if (infer && std::none_of(columns_.begin(), columns_.end(), [&](const Column &c) { return c.name == name; })) {
        if (columns_.size() >= MAX_COLUMNS) fail("too many columns");
        columns_.push_back({name, Kind::Null,record_number_>0});
      }
    }
    row.clear();
    for (const auto &column : columns_) {
      const auto it = fields.find(column.name);
      row.push_back(it == fields.end() ? Value{} : it->second);
    }
    ++record_number_;
    return true;
  }

  Options options_;
  std::string secure_root_;
  std::string path_;
  std::string error_;
  FILE *file_ = nullptr;
  struct stat identity_ {};
  uint64_t record_number_ = 0;
  std::vector<Column> columns_;
  std::unique_ptr<parquet::Reader> parquet_;
  std::vector<std::string> glob_paths_;
  size_t glob_index_ = 0;
};

}}} // namespace oceanbase::sql::file_sql
