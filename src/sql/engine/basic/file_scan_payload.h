/* Copyright (c) 2025 OceanBase. Licensed under the Apache License, Version 2.0. */
#pragma once
#include "sql/engine/basic/file_scan_reader.h"
#include <rapidjson/stringbuffer.h>
#include <rapidjson/writer.h>

namespace oceanbase { namespace sql { namespace file_sql {
inline constexpr char PREFIX[] = "seekdb:file-scan:v1:";
inline bool is_payload(const char *ptr, size_t size)
{ return ptr && size >= sizeof(PREFIX) - 1 && std::memcmp(ptr, PREFIX, sizeof(PREFIX) - 1) == 0; }

inline std::string encode(const Options &options, const Reader &reader)
{
  rapidjson::StringBuffer buffer;
  rapidjson::Writer<rapidjson::StringBuffer> writer(buffer);
  const auto string = [&](const char *key, const std::string &value) {
    writer.Key(key); writer.String(value.data(), static_cast<rapidjson::SizeType>(value.size()));
  };
  writer.StartObject();
  string("path", options.path); string("format", options.format); string("nullstr", options.nullstr);
  writer.Key("delimiter"); writer.Uint(static_cast<unsigned char>(options.delimiter));
  writer.Key("quote"); writer.Uint(static_cast<unsigned char>(options.quote));
  writer.Key("escape"); writer.Uint(static_cast<unsigned char>(options.escape));
  writer.Key("header"); writer.Bool(options.header);
  writer.Key("columns"); writer.StartArray();
  for (const auto &column : reader.columns()) {
    writer.StartArray(); writer.String(column.name.data(), static_cast<rapidjson::SizeType>(column.name.size()));
    writer.Int(static_cast<int>(column.kind)); writer.Bool(column.nullable); writer.Int(column.scale); writer.EndArray();
  }
  writer.EndArray();
  writer.Key("identity"); writer.StartArray();
  const auto &s = reader.identity();
  for (int64_t n : {static_cast<int64_t>(s.st_dev), static_cast<int64_t>(s.st_ino),
                   static_cast<int64_t>(s.st_size), static_cast<int64_t>(s.st_mtim.tv_sec),
                   static_cast<int64_t>(s.st_mtim.tv_nsec), static_cast<int64_t>(s.st_ctim.tv_sec),
                   static_cast<int64_t>(s.st_ctim.tv_nsec)}) writer.Int64(n);
  writer.EndArray(); writer.EndObject();
  return std::string(PREFIX) + std::string(buffer.GetString(), buffer.GetSize());
}

inline Options decode(const char *ptr, size_t size)
{
  if (!is_payload(ptr, size)) throw std::runtime_error("invalid file scan plan");
  rapidjson::Document doc;
  doc.Parse(ptr + sizeof(PREFIX) - 1, size - sizeof(PREFIX) + 1);
  if (doc.HasParseError() || !doc.IsObject()) throw std::runtime_error("invalid file scan plan");
  Options options;
  for (auto entry : {std::make_pair("path", &options.path), {"format", &options.format}, {"nullstr", &options.nullstr}}) {
    if (!doc.HasMember(entry.first) || !doc[entry.first].IsString()) throw std::runtime_error("invalid file scan option");
    entry.second->assign(doc[entry.first].GetString(), doc[entry.first].GetStringLength());
  }
  for (auto entry : {std::make_pair("delimiter", &options.delimiter), {"quote", &options.quote}, {"escape", &options.escape}}) {
    if (!doc.HasMember(entry.first) || !doc[entry.first].IsUint() || doc[entry.first].GetUint() > 127)
      throw std::runtime_error("invalid file scan delimiter");
    *entry.second = static_cast<char>(doc[entry.first].GetUint());
  }
  if (!doc.HasMember("header") || !doc["header"].IsBool()) throw std::runtime_error("invalid file scan header option");
  options.header = doc["header"].GetBool();
  return options;
}

// Compile-time and execution-time metadata must agree, including file identity.
inline bool matches(const std::string &payload, const Reader &reader)
{ return payload == encode(decode(payload.data(), payload.size()), reader); }

struct Cursor {
  Cursor(Options options, const std::string &root) : reader(std::move(options), root) {}
  Reader reader;
  std::vector<Value> row;
};
}}}
