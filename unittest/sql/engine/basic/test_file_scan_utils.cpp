/* Copyright (c) 2025 OceanBase. Licensed under the Apache License, Version 2.0. */
#include "sql/engine/basic/file_scan_payload.h"
#include "sql/engine/basic/file_scan_export.h"
#include <gtest/gtest.h>
#include <fstream>

using namespace oceanbase::sql::file_sql;
class FileScanTest : public ::testing::Test {
protected:
  void SetUp() override { char name[] = "/tmp/seekdb-file-scan-XXXXXX"; root = ::mkdtemp(name); }
  void TearDown() override { for (const auto &path : files) ::unlink(path.c_str()); ::rmdir(root.c_str()); }
  std::string write(const std::string &name, const std::string &data) {
    std::string path = root + "/" + name;
    std::ofstream out(path, std::ios::binary); out << data; out.close(); files.push_back(path); return path;
  }
  std::string root;
  std::vector<std::string> files;
};
TEST_F(FileScanTest, QuotedRecordsTypesAndRescan) {
  Options options;
  options.format = "csv";
  options.path = write("records.csv", "id,note,id\r\n1,\"你好\nline \"\"two\"\"\",2\r\n3,,4\r\n");
  Reader reader(options, root); reader.open();
  ASSERT_EQ(3u, reader.columns().size());
  EXPECT_EQ("id__2", reader.columns()[2].name);
  EXPECT_EQ(Kind::Int, reader.columns()[0].kind);
  std::vector<Value> row;
  ASSERT_TRUE(reader.next(row)); EXPECT_EQ("你好\nline \"two\"", row[1].text);
  ASSERT_TRUE(reader.next(row)); EXPECT_EQ(Kind::Null, row[1].kind);
  EXPECT_FALSE(reader.next(row));
  reader.rewind(); ASSERT_TRUE(reader.next(row)); EXPECT_EQ("1", row[0].text);
}
TEST_F(FileScanTest, JsonUnionNullAndWidening) {
  Options options; options.format = "jsonl";
  options.path = write("records.jsonl", "{\"id\":1,\"name\":\"a\"}\n{\"id\":2.5,\"extra\":true}\n");
  Reader reader(options, root); reader.open();
  ASSERT_EQ(3u, reader.columns().size()); EXPECT_EQ(Kind::Double, reader.columns()[0].kind);
  std::vector<Value> row;
  ASSERT_TRUE(reader.next(row)); EXPECT_EQ(Kind::Null, row[2].kind);
  ASSERT_TRUE(reader.next(row)); EXPECT_EQ(Kind::Null, row[1].kind); EXPECT_EQ("true", row[2].text);
  EXPECT_FALSE(reader.next(row));
}
TEST_F(FileScanTest, RejectsNestedDuplicateAndMalformedInput) {
  for (const std::string &data : {"{\"x\":[]}\n", "{\"x\":1,\"x\":2}\n", "{broken}\n"}) {
    Options options; options.format = "jsonl"; options.path = write("bad.jsonl", data);
    Reader reader(options, root); EXPECT_THROW(reader.open(), std::runtime_error);
  }
  Options options; options.format = "csv"; options.path = write("bad.csv", "id,name\n1,\"unterminated\n");
  Reader reader(options, root); EXPECT_THROW(reader.open(), std::runtime_error);
}
TEST_F(FileScanTest, DetectsChangesAndChecksRoot) {
  Options options; options.format = "csv"; options.path = write("change.csv", "id\n1\n");
  Reader reader(options, root); reader.open();
  std::ofstream(options.path, std::ios::app) << "2\n";
  std::vector<Value> row; EXPECT_THROW(reader.next(row), std::runtime_error);
  EXPECT_THROW(checked_path(options.path, "/tmp/not-a-real-secure-directory"), std::runtime_error);
  EXPECT_THROW(checked_path("/etc/passwd", root), std::runtime_error);
  EXPECT_THROW(checked_path(options.path, ""), std::runtime_error);
}
TEST_F(FileScanTest, TypeBoundariesAndUtf8) {
  EXPECT_EQ(Kind::Int, classify("9223372036854775807"));
  EXPECT_EQ(Kind::String, classify("9223372036854775808"));
  EXPECT_EQ(Kind::Double, classify("1.25e3"));
  EXPECT_EQ(Kind::Date, classify("2024-02-29"));
  EXPECT_EQ(Kind::String, classify("2023-02-29"));
  EXPECT_EQ(Kind::DateTime, classify("2024-02-29 23:59:59.123456"));
  EXPECT_EQ(Kind::String, classify("2024-02-29 24:00:00"));
  EXPECT_TRUE(valid_utf8("中文😀")); EXPECT_FALSE(valid_utf8(std::string("\xed\xa0\x80", 3)));
}
TEST_F(FileScanTest, PlanMetadataAndGlob) {
  Options options; options.format = "csv"; options.path = write("metadata.csv", "id\n1\n");
  Reader reader(options, root); reader.open();
  const auto payload = encode(options, reader);
  EXPECT_TRUE(matches(payload, reader));
  EXPECT_EQ(options.path, decode(payload.data(), payload.size()).path);
  Options glob_options; glob_options.format = "glob"; glob_options.path = root + "/*.csv";
  Reader files(glob_options, root); files.open();
  std::vector<Value> row;
  ASSERT_TRUE(files.next(row)); EXPECT_EQ(options.path, row[0].text); EXPECT_FALSE(files.next(row));
  files.rewind(); EXPECT_TRUE(files.next(row));
}
TEST_F(FileScanTest, SnappyLiteralOverlapAndMalformedInput) {
  EXPECT_EQ("xababab",parquet::decompress(std::string("\x07\x08xab\x01\x02",7),1,7));
  EXPECT_THROW(parquet::decompress(std::string("\x07\x01\x00",3),1,7),std::runtime_error);
  EXPECT_THROW(parquet::decompress(std::string("\x80",1),1,7),std::runtime_error);
}
TEST_F(FileScanTest, DateTimePrecisionSurvivesPlanMetadata) {
  Options options; options.format="csv";
  options.path=write("precision.csv", "whole,fractional\n2024-02-29 12:00:00,2024-02-29 12:00:00.12\n2024-03-01 12:00:00,2024-03-01 12:00:00.123456\n");
  Reader reader(options,root);reader.open();
  ASSERT_EQ(2u,reader.columns().size());
  EXPECT_EQ(Kind::DateTime,reader.columns()[0].kind);
  EXPECT_EQ(0,reader.columns()[0].scale);
  EXPECT_EQ(6,reader.columns()[1].scale);
  const auto plan=encode(options,reader);
  Reader execution(decode(plan.data(),plan.size()),root);execution.open();
  EXPECT_TRUE(matches(plan,execution));
  std::vector<Value> row;
  ASSERT_TRUE(execution.next(row));
  EXPECT_EQ("2024-02-29 12:00:00",row[0].text);
  EXPECT_EQ("2024-02-29 12:00:00.12",row[1].text);
}
TEST_F(FileScanTest, CopyRoundTripNoOverwriteAndCleanup) {
  ExportOptions options;options.csv.path=root+"/output.csv";options.csv.nullstr="NULL";options.csv.header=true;options.names={"a","b"};
  files.push_back(options.csv.path);
  {
    Exporter exporter(options,root);
    exporter.row({{"line\n\"quoted\"",Kind::String},{"",Kind::Null}});
    exporter.row({{"NULL",Kind::String},{"",Kind::String}});
    exporter.finish();
  }
  options.csv.format="csv";Reader reader(options.csv,root);reader.open();std::vector<Value> row;
  ASSERT_TRUE(reader.next(row));EXPECT_EQ("line\n\"quoted\"",row[0].text);EXPECT_EQ(Kind::Null,row[1].kind);
  ASSERT_TRUE(reader.next(row));EXPECT_EQ("NULL",row[0].text);EXPECT_EQ(Kind::String,row[0].kind);EXPECT_EQ("",row[1].text);EXPECT_EQ(Kind::String,row[1].kind);
  EXPECT_THROW(Exporter(options,root),std::runtime_error);
  options.csv.path=root+"/failed.csv";
  {Exporter exporter(options,root);exporter.row({{"partial",Kind::String}});}
  EXPECT_NE(0,access(options.csv.path.c_str(),F_OK));
  glob_t matches{};ASSERT_EQ(GLOB_NOMATCH,glob((root+"/.seekdb-copy-*").c_str(),0,nullptr,&matches));globfree(&matches);
}
int main(int argc, char **argv) { ::testing::InitGoogleTest(&argc, argv); return RUN_ALL_TESTS(); }
