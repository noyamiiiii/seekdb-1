/* Copyright (c) 2025 OceanBase. Licensed under the Apache License, Version 2.0. */
#pragma once
#include "sql/engine/basic/file_scan_types.h"
#include <algorithm>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <ctime>
#include <limits>
#include <map>
#include <stdexcept>
#include <vector>
#include <zlib.h>

namespace oceanbase { namespace sql { namespace file_sql { namespace parquet {
// Parquet uses the Apache Thrift compact protocol for its footer and page
// headers. All lengths are checked before allocating or advancing a cursor.
constexpr size_t LIMIT=64*1024*1024;
[[noreturn]] inline void fail(const char *message){throw std::runtime_error(std::string("file SQL: Parquet: ")+message);}
struct Bytes {
  const uint8_t *p;size_t size,pos=0;
  Bytes(const std::string &s):p(reinterpret_cast<const uint8_t*>(s.data())),size(s.size()){}
  Bytes(const uint8_t *data,size_t count):p(data),size(count){}
  void need(size_t n)const{if(n>size-pos)fail("truncated input");}
  uint8_t byte(){need(1);return p[pos++];}
  uint64_t little(unsigned n){if(n>8)fail("invalid integer width");need(n);uint64_t v=0;for(unsigned i=0;i<n;++i)v|=uint64_t(p[pos++])<<(8*i);return v;}
  uint64_t varint(){uint64_t n=0;for(unsigned bit=0;bit<64;bit+=7){auto b=byte();if(bit==63&&b>1)fail("varint overflow");n|=uint64_t(b&127)<<bit;if(!(b&128))return n;}fail("invalid varint");}
  int64_t zigzag(){const auto n=varint();return static_cast<int64_t>(n>>1)^-static_cast<int64_t>(n&1);}
  std::string take(size_t n){need(n);std::string s(reinterpret_cast<const char*>(p+pos),n);pos+=n;return s;}
};
struct Node {
  int64_t number=0;std::string text;std::vector<Node> list;std::map<int,Node> fields;
  bool has(int id)const{return fields.count(id)!=0;}
  const Node &at(int id)const{auto it=fields.find(id);if(it==fields.end())fail("missing required metadata field");return it->second;}
  int64_t integer(int id,int64_t fallback=0)const{return has(id)?at(id).number:fallback;}
};
inline Node compact(Bytes &b,int type=12,unsigned depth=0){
  if(depth>32)fail("metadata nesting limit exceeded");Node n;
  switch(type){
    case 1:n.number=1;break;case 2:n.number=0;break;case 3:n.number=static_cast<int8_t>(b.byte());break;
    case 4:case 5:case 6:n.number=b.zigzag();break;
    case 7:b.little(8);break;
    case 8:{auto length=b.varint();if(length>LIMIT)fail("metadata string limit exceeded");n.text=b.take(length);break;}
    case 9:case 10:{auto head=b.byte();uint64_t count=head>>4;if(count==15)count=b.varint();if(count>1000000)fail("metadata list limit exceeded");n.list.reserve(count);for(uint64_t i=0;i<count;++i)n.list.push_back(compact(b,head&15,depth+1));break;}
    case 11:{auto count=b.varint();if(count>1000000)fail("metadata map limit exceeded");if(count){auto types=b.byte();for(uint64_t i=0;i<count;++i){compact(b,types>>4,depth+1);compact(b,types&15,depth+1);}}break;}
    case 12:{int previous=0;for(;;){auto head=b.byte();if(!head)break;int id=(head>>4)?previous+(head>>4):static_cast<int>(b.zigzag());if(id<=0||!n.fields.emplace(id,compact(b,head&15,depth+1)).second)fail("duplicate metadata field");previous=id;}break;}
    default:fail("unknown compact protocol type");
  }return n;
}
inline std::string read(FILE *file,uint64_t offset,size_t length,uint64_t file_size){
  if(length>LIMIT||offset>file_size||length>file_size-offset)fail("file range exceeds bounds");
  if(fseeko(file,static_cast<off_t>(offset),SEEK_SET))fail("cannot seek input");
  std::string data(length,'\0');if(length&&fread(&data[0],1,length,file)!=length)fail("cannot read input");return data;
}
inline std::string decompress(const std::string &input,int codec,size_t expected){
  if(expected>LIMIT)fail("page exceeds memory limit");
  if(codec==0){if(input.size()!=expected)fail("incorrect uncompressed page size");return input;}
  if(codec==1){
    Bytes b(input);if(b.varint()!=expected)fail("incorrect Snappy output length");std::string out;out.reserve(expected);
    while(b.pos<b.size){auto tag=b.byte();size_t length=0,offset=0;
      if((tag&3)==0){unsigned size=tag>>2;length=size<60?size+1:b.little(size-59)+1;if(length>expected-out.size())fail("Snappy literal overflow");out+=b.take(length);continue;}
      if((tag&3)==1){length=4+((tag>>2)&7);offset=((tag&0xe0)<<3)|b.byte();}
      else{length=1+(tag>>2);offset=b.little((tag&3)==2?2:4);}
      if(!offset||offset>out.size()||length>expected-out.size())fail("invalid Snappy backreference");
      for(size_t i=0;i<length;++i)out+=out[out.size()-offset];
    }
    if(out.size()!=expected)fail("truncated Snappy page");return out;
  }
  if(codec==2){
    std::string out(expected,'\0');z_stream z{};z.next_in=reinterpret_cast<Bytef*>(const_cast<char*>(input.data()));z.avail_in=input.size();z.next_out=reinterpret_cast<Bytef*>(out.data());z.avail_out=out.size();
    if(inflateInit2(&z,15+32)!=Z_OK)fail("cannot initialize gzip decoder");const int ret=inflate(&z,Z_FINISH);const auto count=z.total_out;inflateEnd(&z);
    if(ret!=Z_STREAM_END||count!=expected)fail("invalid gzip page");return out;
  }
  fail("unsupported compression codec");
}
inline std::vector<uint32_t> hybrid(Bytes &b,size_t count,unsigned width){
  if(width>32||count>1000000)fail("RLE width or value count exceeds limit");std::vector<uint32_t> out;out.reserve(count);
  while(out.size()<count){uint64_t run=b.varint();if(!run)fail("empty RLE run");
    if(run&1){uint64_t number=(run>>1)*8;if(number>1000008)fail("bit packed run exceeds limit");const size_t bytes=(number*width+7)/8;b.need(bytes);const size_t start=b.pos;
      for(uint64_t i=0;i<number&&out.size()<count;++i){uint32_t v=0;for(unsigned bit=0;bit<width;++bit){const uint64_t index=i*width+bit;v|=uint32_t((b.p[start+index/8]>>(index%8))&1)<<bit;}out.push_back(v);}b.pos+=bytes;
    }else{uint64_t number=run>>1;const uint32_t value=b.little((width+7)/8);if(!number||number>1000000)fail("RLE run exceeds limit");for(uint64_t i=0;i<number&&out.size()<count;++i)out.push_back(value);}
  }return out;
}
struct Schema {std::string name;int type,converted=-1,length=0;bool optional=false;};
inline Kind kind(const Schema &s){
  if(s.converted==6)return Kind::Date;if(s.converted==9||s.converted==10||s.type==3)return Kind::DateTime;
  if(s.type==0)return Kind::Bool;if(s.type==1||s.type==2)return s.converted==14?Kind::String:Kind::Int;
  return s.type==4||s.type==5?Kind::Double:Kind::String;
}
inline std::string timestamp(int64_t micros,bool date){
  int64_t seconds=micros/1000000,remainder=micros%1000000;if(remainder<0){remainder+=1000000;--seconds;}
  time_t time=seconds;struct tm value{};if(!gmtime_r(&time,&value))fail("timestamp out of range");
  char buffer[64];if(!strftime(buffer,sizeof(buffer),date?"%Y-%m-%d":"%Y-%m-%d %H:%M:%S",&value))fail("invalid timestamp");
  std::string out=buffer;if(!date&&remainder){char fraction[8];snprintf(fraction,sizeof(fraction),".%06d",int(remainder));out+=fraction;}return out;
}
inline std::vector<Value> plain(Bytes &b,size_t count,const Schema &s){
  if(count>1000000)fail("too many values in one page");std::vector<Value> out;out.reserve(count);size_t boolean_start=b.pos;
  for(size_t i=0;i<count;++i){Value v;v.kind=kind(s);uint64_t integer=0;
    switch(s.type){
      case 0:b.need((count+7)/8);v.text=(b.p[boolean_start+i/8]>>(i%8))&1?"true":"false";break;
      case 1:integer=b.little(4);v.text=s.converted>=11&&s.converted<=13?std::to_string(integer):std::to_string(static_cast<int32_t>(integer));break;
      case 2:integer=b.little(8);v.text=s.converted==14?std::to_string(integer):std::to_string(static_cast<int64_t>(integer));break;
      case 3:{const auto nanos=b.little(8);const auto day=b.little(4);if(nanos>=86400000000000ULL)fail("invalid INT96 timestamp");v.text=timestamp((static_cast<int64_t>(day)-2440588)*86400000000LL+nanos/1000,false);break;}
      case 4:case 5:{double value;if(s.type==4){uint32_t bits=b.little(4);float number;memcpy(&number,&bits,4);value=number;}else{uint64_t bits=b.little(8);memcpy(&value,&bits,8);}if(!std::isfinite(value))fail("non-finite floating point value");char text[64];snprintf(text,sizeof(text),"%.17g",value);v.text=text;break;}
      case 6:{auto length=b.little(4);v.text=b.take(length);break;}
      case 7:if(s.length<=0)fail("invalid fixed byte width");v.text=b.take(s.length);break;
      default:fail("unsupported physical type");
    }
    if(s.converted==6){if(s.type!=1)fail("invalid DATE physical type");v.text=timestamp(static_cast<int64_t>(static_cast<int32_t>(integer))*86400000000LL,true);}
    if(s.converted==9||s.converted==10){int64_t micros=static_cast<int64_t>(integer);if(s.converted==9){if(micros>INT64_MAX/1000||micros<INT64_MIN/1000)fail("timestamp overflow");micros*=1000;}v.text=timestamp(micros,false);}
    out.push_back(std::move(v));
  }
  if(s.type==0)b.pos=boolean_start+(count+7)/8;return out;
}
class Reader {
  struct Chunk {const Node *metadata=nullptr;uint64_t offset=0;std::vector<Value> dictionary,values;size_t index=0;};
  FILE *file_;uint64_t size_,footer_start_;Node footer_;std::vector<Schema> schema_;std::vector<Chunk> chunks_;
  size_t group_=0;int64_t row_=0,rows_=0;bool initialized_=false;
  bool projected_=false;std::vector<size_t> projection_;
  void group(){
    chunks_.clear();const auto &groups=footer_.at(4).list;
    while(group_<groups.size()&&groups[group_].integer(3)==0)++group_;
    if(group_==groups.size()){rows_=0;return;}
    const auto &g=groups[group_];rows_=g.integer(3);row_=0;if(rows_<0||g.at(1).list.size()!=schema_.size())fail("invalid row group");
    for(const auto &column:g.at(1).list){if(column.has(1)&&!column.at(1).text.empty())fail("external column chunks are unsupported");const auto &m=column.at(3);
      if(m.at(3).list.size()!=1||m.integer(1)!=schema_[chunks_.size()].type||m.integer(5)!=rows_)fail("column metadata does not match schema");
      Chunk c;c.metadata=&m;c.offset=m.has(11)?std::min(m.integer(9),m.integer(11)):m.integer(9);if(c.offset<4||c.offset>=footer_start_)fail("invalid column chunk offset");chunks_.push_back(std::move(c));
    }
  }
  void page(size_t column){
    auto &c=chunks_[column];const auto &s=schema_[column];
    for(unsigned pages=0;pages<10000;++pages){
      auto header=read(file_,c.offset,std::min<uint64_t>(65536,footer_start_-c.offset),size_);Bytes h(header);Node p=compact(h);
      int64_t compressed=p.integer(3,-1),uncompressed=p.integer(2,-1);if(compressed<0||uncompressed<0||compressed>LIMIT||uncompressed>LIMIT)fail("invalid page size");
      c.offset+=h.pos;auto input=read(file_,c.offset,compressed,footer_start_);c.offset+=compressed;
      const int type=p.integer(1,-1),codec=c.metadata->integer(4);
      if(type==2){auto data=decompress(input,codec,uncompressed);Bytes b(data);const auto &d=p.at(7);if(d.integer(2)!=0)fail("dictionary must use PLAIN encoding");c.dictionary=plain(b,d.integer(1),s);continue;}
      if(type!=0&&type!=3)continue;
      const auto &d=p.at(type==0?5:8);const int64_t count=d.integer(1);if(count<0||count>1000000)fail("invalid page value count");
      std::string data;size_t level_bytes=0;int encoding=d.integer(type==0?2:4);
      if(type==3){int64_t definition=d.integer(5),repetition=d.integer(6);if(definition<0||repetition!=0||definition>compressed||definition>uncompressed)fail("invalid V2 level lengths");level_bytes=definition;
        data=input.substr(0,level_bytes);data+=decompress(input.substr(level_bytes),d.integer(7,1)?codec:0,uncompressed-level_bytes);
      }else data=decompress(input,codec,uncompressed);
      Bytes b(data);std::vector<uint32_t> levels(count,1);
      if(s.optional){if(type==0)level_bytes=b.little(4);b.need(level_bytes);Bytes level(b.p+b.pos,level_bytes);levels=hybrid(level,count,1);b.pos+=level_bytes;}
      else if(level_bytes)fail("unexpected definition levels on required column");
      size_t nonnull=0;for(auto value:levels){if(value>1)fail("invalid definition level");nonnull+=value;}
      std::vector<Value> values;
      if(nonnull==0){}
      else if(encoding==0)values=plain(b,nonnull,s);
      else if(encoding==2||encoding==8){auto indices=hybrid(b,nonnull,b.byte());values.reserve(nonnull);for(auto index:indices){if(index>=c.dictionary.size())fail("dictionary index out of range");values.push_back(c.dictionary[index]);}}
      else if(encoding==3&&s.type==0){const auto length=b.little(4);b.need(length);Bytes encoded(b.p+b.pos,length);auto bits=hybrid(encoded,nonnull,1);for(auto bit:bits){if(bit>1)fail("invalid boolean value");values.push_back({bit?"true":"false",Kind::Bool});}}
      else fail("unsupported data encoding");
      c.values.clear();c.values.reserve(count);c.index=0;size_t value=0;for(auto level:levels)c.values.push_back(level?std::move(values[value++]):Value{});
      size_t cache=0;for(const auto &chunk:chunks_){
        cache+=(chunk.values.size()+chunk.dictionary.size())*sizeof(Value);
        for(const auto &v:chunk.values)cache+=v.text.size();for(const auto &v:chunk.dictionary)cache+=v.text.size();
        if(cache>2*LIMIT)fail("decoded page cache limit exceeded");
      }
      if(!c.values.empty())return;
    }fail("too many empty or index pages");
  }
public:
  Reader(FILE *file,uint64_t size):file_(file),size_(size){
    if(size<12||read(file,0,4,size)!="PAR1"||read(file,size-4,4,size)!="PAR1")fail("invalid magic bytes");
    auto trailer=read(file,size-8,4,size);Bytes t(trailer);auto length=t.little(4);if(length>16*1024*1024||length>size-12)fail("footer exceeds bounds");
    footer_start_=size-8-length;auto metadata=read(file,footer_start_,length,size);Bytes b(metadata);footer_=compact(b);
    const auto &elements=footer_.at(2).list;if(elements.empty()||elements[0].integer(5)!=static_cast<int64_t>(elements.size()-1)||elements.size()>4097)fail("only flat Parquet schemas are supported");
    for(size_t i=1;i<elements.size();++i){const auto &n=elements[i];if(!n.has(1)||n.has(5)||n.integer(3)>1)fail("nested or repeated columns are unsupported");
      Schema s;s.name=n.at(4).text;s.type=n.integer(1,-1);s.length=n.integer(2);s.converted=n.integer(6,-1);s.optional=n.integer(3)==1;
      if(n.has(10)){const auto &logical=n.at(10);if(logical.has(6))s.converted=6;if(logical.has(8)){const auto &unit=logical.at(8).at(2);if(unit.has(1))s.converted=9;else if(unit.has(2))s.converted=10;else fail("unsupported timestamp unit");}}
      if(s.converted==5)fail("decimal logical columns are unsupported");schema_.push_back(std::move(s));
    }
    int64_t total=0;for(const auto &g:footer_.at(4).list){auto rows=g.integer(3,-1);if(rows<0||rows>INT64_MAX-total)fail("invalid row count");total+=rows;}if(total!=footer_.integer(3,-1))fail("inconsistent file row count");
  }
  std::vector<Column> columns()const{std::vector<Column> out;for(const auto &s:schema_)out.push_back({s.name,kind(s),s.optional,s.converted==10?6:s.converted==9?3:0});return out;}
  void projection(const std::vector<size_t> &columns){projection_=columns;projected_=true;for(auto i:columns)if(i>=schema_.size())fail("projection index out of range");}
  void rewind(){group_=0;row_=rows_=0;initialized_=false;chunks_.clear();}
  bool next(std::vector<Value> &row){
    if(!initialized_){group();initialized_=true;}
    if(row_==rows_){if(group_>=footer_.at(4).list.size())return false;++group_;group();}
    if(!rows_)return false;row.clear();row.reserve(schema_.size());
    for(size_t i=0;i<schema_.size();++i){
      if(projected_&&std::find(projection_.begin(),projection_.end(),i)==projection_.end()){row.push_back(Value{});continue;}
      auto &c=chunks_[i];if(c.index==c.values.size()){
        try{page(i);}catch(const std::exception &e){throw std::runtime_error(std::string(e.what())+" (column "+schema_[i].name+")");}
      }row.push_back(c.values[c.index++]);
    }
    ++row_;return true;
  }
};
}}}} // namespace oceanbase::sql::file_sql::parquet
