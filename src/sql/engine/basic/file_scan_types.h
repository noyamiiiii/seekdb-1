/* Copyright (c) 2025 OceanBase. Licensed under the Apache License, Version 2.0. */
#pragma once
#include <string>
namespace oceanbase { namespace sql { namespace file_sql {
enum class Kind { Null, Int, Double, Bool, String, Date, DateTime };
struct Value { std::string text; Kind kind=Kind::Null; };
struct Column { std::string name; Kind kind=Kind::Null;bool nullable=false;int scale=0; };
struct Options {
  std::string path,format,nullstr;
  char delimiter=',',quote='"',escape='"';
  bool header=true;
};
}}}
