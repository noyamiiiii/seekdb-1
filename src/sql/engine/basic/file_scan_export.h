/* Copyright (c) 2025 OceanBase. Licensed under the Apache License, Version 2.0. */
#pragma once
#include "sql/engine/basic/file_scan_reader.h"
#include <atomic>
#include <rapidjson/stringbuffer.h>
#include <rapidjson/writer.h>
namespace oceanbase { namespace sql { namespace file_sql {
inline constexpr char EXPORT_PREFIX[]="seekdb:copy-csv:v1:";
inline bool is_export(const char *p,size_t n){return p&&n>=sizeof(EXPORT_PREFIX)-1&&!memcmp(p,EXPORT_PREFIX,sizeof(EXPORT_PREFIX)-1);}
struct ExportOptions {Options csv;std::vector<std::string> names;};
inline std::string export_plan(const ExportOptions &o){
  rapidjson::StringBuffer b;rapidjson::Writer<rapidjson::StringBuffer>w(b);w.StartObject();
  w.Key("path");w.String(o.csv.path.data(),o.csv.path.size());w.Key("nullstr");w.String(o.csv.nullstr.data(),o.csv.nullstr.size());
  w.Key("delimiter");w.Uint(static_cast<unsigned char>(o.csv.delimiter));w.Key("quote");w.Uint(static_cast<unsigned char>(o.csv.quote));w.Key("escape");w.Uint(static_cast<unsigned char>(o.csv.escape));w.Key("header");w.Bool(o.csv.header);
  w.Key("names");w.StartArray();for(const auto &name:o.names)w.String(name.data(),name.size());w.EndArray();w.EndObject();return std::string(EXPORT_PREFIX)+std::string(b.GetString(),b.GetSize());
}
inline ExportOptions export_options(const char *p,size_t n){
  if(!is_export(p,n))throw std::runtime_error("file SQL: invalid COPY plan");
  rapidjson::Document d;d.Parse(p+sizeof(EXPORT_PREFIX)-1,n-sizeof(EXPORT_PREFIX)+1);
  if(d.HasParseError()||!d.IsObject()||!d.HasMember("path")||!d["path"].IsString()||!d.HasMember("nullstr")||!d["nullstr"].IsString()||!d.HasMember("names")||!d["names"].IsArray()||!d.HasMember("header")||!d["header"].IsBool())throw std::runtime_error("file SQL: invalid COPY plan");
  ExportOptions o;o.csv.path.assign(d["path"].GetString(),d["path"].GetStringLength());o.csv.nullstr.assign(d["nullstr"].GetString(),d["nullstr"].GetStringLength());o.csv.header=d["header"].GetBool();
  for(auto entry:{std::make_pair("delimiter",&o.csv.delimiter),{"quote",&o.csv.quote},{"escape",&o.csv.escape}}){if(!d.HasMember(entry.first)||!d[entry.first].IsUint()||d[entry.first].GetUint()>127||d[entry.first].GetUint()==0)throw std::runtime_error("file SQL: invalid COPY delimiter");*entry.second=char(d[entry.first].GetUint());}
  for(const auto &name:d["names"].GetArray()){if(!name.IsString())throw std::runtime_error("file SQL: invalid COPY name");o.names.emplace_back(name.GetString(),name.GetStringLength());}return o;
}
class Exporter {
  ExportOptions options_;int directory_=-1;FILE *stream_=nullptr;std::string target_,temporary_;bool owns_temporary_=false;
  void write(const std::string &s){if(s.size()&&fwrite(s.data(),1,s.size(),stream_)!=s.size())throw std::runtime_error("file SQL: cannot write COPY output");}
public:
  Exporter(ExportOptions options,const std::string &secure):options_(std::move(options)){
    const auto &path=options_.csv.path;
    if(path.find('\0')!=std::string::npos)throw std::runtime_error("file SQL: invalid COPY path");
    auto slash=path.rfind('/');target_=slash==std::string::npos?path:path.substr(slash+1);
    if(target_.empty()||target_=="."||target_=="..")throw std::runtime_error("file SQL: invalid COPY filename");
    const std::string parent=checked_path(slash==std::string::npos?".":path.substr(0,slash+1),secure);
    directory_=::open(parent.c_str(),O_RDONLY|O_DIRECTORY|O_CLOEXEC|O_NOFOLLOW);
    if(directory_<0)throw std::runtime_error("file SQL: cannot open COPY directory");
    try{
      if(canonical("/proc/self/fd/"+std::to_string(directory_))!=parent)throw std::runtime_error("file SQL: COPY directory changed");
      struct stat existing;if(fstatat(directory_,target_.c_str(),&existing,AT_SYMLINK_NOFOLLOW)==0||errno!=ENOENT)throw std::runtime_error("file SQL: COPY target already exists or cannot be inspected");
      static std::atomic<uint64_t> sequence{0};
      for(unsigned retry=0;retry<100;++retry){temporary_=".seekdb-copy-"+std::to_string(getpid())+"-"+std::to_string(sequence.fetch_add(1));const int fd=openat(directory_,temporary_.c_str(),O_WRONLY|O_CREAT|O_EXCL|O_CLOEXEC|O_NOFOLLOW,0600);if(fd>=0){owns_temporary_=true;stream_=fdopen(fd,"wb");if(!stream_){close(fd);throw std::runtime_error("file SQL: cannot create COPY stream");}break;}if(errno!=EEXIST)throw std::runtime_error("file SQL: cannot create COPY output");}
      if(!stream_)throw std::runtime_error("file SQL: cannot allocate a temporary COPY filename");
      if(options_.csv.header){std::vector<Value> names;for(const auto &name:options_.names)names.push_back({name,Kind::String});row(names);}
    }catch(...){if(stream_){fclose(stream_);stream_=nullptr;}if(owns_temporary_)unlinkat(directory_,temporary_.c_str(),0);close(directory_);directory_=-1;throw;}
  }
  ~Exporter(){if(stream_)fclose(stream_);if(directory_>=0){if(owns_temporary_)unlinkat(directory_,temporary_.c_str(),0);close(directory_);}}
  void row(const std::vector<Value> &values){
    for(size_t i=0;i<values.size();++i){if(i)write(std::string(1,options_.csv.delimiter));const auto &v=values[i];if(v.kind==Kind::Null){write(options_.csv.nullstr);continue;}
      const bool quoted=v.text==options_.csv.nullstr||v.text.find_first_of(std::string()+options_.csv.delimiter+options_.csv.quote+"\r\n")!=std::string::npos;
      if(quoted)write(std::string(1,options_.csv.quote));
      std::string field;for(char c:v.text){if(quoted&&(c==options_.csv.quote||(options_.csv.escape!=options_.csv.quote&&c==options_.csv.escape)))field+=options_.csv.escape;field+=c;}write(field);
      if(quoted)write(std::string(1,options_.csv.quote));
    }write("\n");
  }
  void finish(){
    if(fflush(stream_)||fsync(fileno(stream_)))throw std::runtime_error("file SQL: cannot flush COPY output");
    if(fclose(stream_)){stream_=nullptr;throw std::runtime_error("file SQL: cannot close COPY output");}stream_=nullptr;
    // linkat publishes atomically and fails if any target, including a symlink,
    // appeared since validation. The directory descriptor pins the parent.
    if(linkat(directory_,temporary_.c_str(),directory_,target_.c_str(),0))throw std::runtime_error("file SQL: cannot publish COPY output; target may already exist");
    if(unlinkat(directory_,temporary_.c_str(),0)==0){owns_temporary_=false;temporary_.clear();}
  }
};
}}}
