#!/usr/bin/env python3
"""Compile the actual adaptor snapshot code with a small storage/GPU API double."""
from pathlib import Path
import subprocess, tempfile
root=Path(__file__).resolve().parents[4]
source=(root/'src/oblib/lib/vector/ob_vsag_adaptor.cpp').read_text()
section=source[source.index('static const char OB_CUVS_SNAPSHOT_MAGIC'):source.index('}  // anonymous namespace')]
prefix=r'''
#include <algorithm>
#include <cassert>
#include <chrono>
#include <cstdio>
#include <cstring>
#include <fstream>
#include <istream>
#include <limits>
#include <map>
#include <memory>
#include <mutex>
#include <ostream>
#include <pthread.h>
#include <set>
#include <sstream>
#include <unistd.h>
#include <vector>
#define LOG_WARN(...) ((void)0)
#define LOG_INFO(...) ((void)0)
enum {OB_SUCCESS=0,OB_ERR_VSAG_RETURN_ERROR=-1,OB_ERR_UNEXPECTED=-2,OB_CUVS_MIN_PTS=2};
struct ObTimeUtility {static int64_t current_time(){return 0;}};
struct Gpu {int64_t n,dim;std::vector<float> data;};
static int live_gpu=0,build_calls=0;static bool fail_export=false;
static void seekdb_cuvs_free(void* p){if(p){delete static_cast<Gpu*>(p);--live_gpu;}}
static std::shared_ptr<void> ob_cuvs_wrap(void* p){return std::shared_ptr<void>(p,seekdb_cuvs_free);}
static std::shared_ptr<void> ob_cuvs_build_big_stack(const float* v,long n,long d){++build_calls;++live_gpu;return ob_cuvs_wrap(new Gpu{n,d,std::vector<float>(v,v+n*d)});}
static int seekdb_cuvs_serialize(void* p,const char* path){auto* g=static_cast<Gpu*>(p);std::ofstream f(path,std::ios::binary);f.write((char*)&g->n,8);f.write((char*)&g->dim,8);f.write((char*)g->data.data(),g->data.size()*4);return f?0:1;}
static void* seekdb_cuvs_deserialize(const char* path,long n,long d){std::ifstream f(path,std::ios::binary);int64_t a=0,b=0;f.read((char*)&a,8);f.read((char*)&b,8);if(!f||a!=n||b!=d)return nullptr;std::vector<float> v(n*d);f.read((char*)v.data(),v.size()*4);if(!f)return nullptr;++live_gpu;return new Gpu{n,d,std::move(v)};}
static int seekdb_cuvs_export_dataset(void* p,float* v,long n,long d){if(fail_export)return 1;auto*g=static_cast<Gpu*>(p);assert(g->n==n&&g->dim==d);std::copy(g->data.begin(),g->data.end(),v);return 0;}
struct ObCuvsEntry {std::shared_ptr<void> bridge_;int dim_=0;std::vector<int64_t> ids_,buf_ids_;std::vector<float> buf_vecs_;size_t built_n_=0;bool vsag_ready_=false;};
static std::mutex g_ob_cuvs_mu,g_ob_cuvs_marked_mu;
static std::map<void*,ObCuvsEntry*> g_ob_cuvs_reg;
static std::set<void*> g_ob_cuvs_marked;
'''
tests=r'''
static void clear(){for(auto&p:g_ob_cuvs_reg)delete p.second;g_ob_cuvs_reg.clear();g_ob_cuvs_marked.clear();assert(live_gpu==0);}
static int load(const std::string& s,int dim){std::istringstream in(s.substr(8),std::ios::binary);return ob_cuvs_deserialize((void*)2,dim,in);}
int main(){
 auto* e=new ObCuvsEntry;e->dim_=7;for(int i=0;i<64;++i){e->buf_ids_.push_back(i*17-100);for(int j=0;j<7;++j)e->buf_vecs_.push_back(float(i*7+j)/19);}
 auto ids=e->buf_ids_;auto vectors=e->buf_vecs_;g_ob_cuvs_reg[(void*)1]=e;
 int ret=-99;std::ostringstream out(std::ios::binary);assert(ob_cuvs_serialize((void*)1,out,ret)&&ret==0);std::string compact=out.str();assert(build_calls==1);
 int32_t flag;memcpy(&flag,compact.data()+20,4);assert(flag==1);assert(compact.size()==32+ids.size()*8+16+vectors.size()*4);
 clear();assert(load(compact,7)==0);auto*r=g_ob_cuvs_reg.at((void*)2);assert(r->buf_ids_==ids&&r->ids_==ids&&r->buf_vecs_==vectors&&r->built_n_==ids.size());assert(build_calls==1);
 std::ostringstream again(std::ios::binary);assert(ob_cuvs_serialize((void*)2,again,ret)&&ret==0&&again.str()==compact);clear();
 // Legacy flag-zero fixture retains its original duplicate vector payload.
 std::string legacy=compact;flag=0;memcpy(legacy.data()+20,&flag,4);legacy.insert(32+ids.size()*8,(char*)vectors.data(),vectors.size()*4);
 assert(load(legacy,7)==0);assert(g_ob_cuvs_reg.at((void*)2)->buf_vecs_==vectors);assert(build_calls==1);clear();
 fail_export=true;assert(load(compact,7)!=0);assert(g_ob_cuvs_reg.empty()&&live_gpu==0);fail_export=false;
 assert(load(compact,8)!=0);assert(live_gpu==0);
 for(size_t n:{size_t(8),size_t(20),size_t(31),size_t(40),compact.size()-1}){assert(load(compact.substr(0,n),7)!=0);assert(g_ob_cuvs_reg.empty()&&live_gpu==0);}
 std::string bad=compact;flag=2;memcpy(bad.data()+20,&flag,4);assert(load(bad,7)!=0);assert(live_gpu==0);
 puts("PASS adaptor compact/legacy snapshots, byte-identical reserialization, original IDs/vectors, no rebuild, malformed/truncated payloads, export-failure cleanup");
}
'''
with tempfile.TemporaryDirectory(prefix='seekdb-snapshot-contract-') as d:
 p=Path(d);(p/'contract.cpp').write_text(prefix+section+tests)
 subprocess.run(['c++','-std=c++17','-O1','-g','-pthread','-fsanitize=address,undefined','-fno-omit-frame-pointer',str(p/'contract.cpp'),'-o',str(p/'contract')],check=True)
 subprocess.run([str(p/'contract')],check=True)
