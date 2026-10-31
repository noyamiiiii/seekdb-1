// Host-only API contract/failure-injection tests; this is not a GPU implementation.
#include <cuvs/neighbors/cagra.h>
#include <cuvs/cuda_runtime.h>
#include <algorithm>
#include <atomic>
#include <cassert>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <fstream>
#include <limits>
#include <map>
#include <mutex>
#include <string>
#include <thread>
#include <vector>
#include <chrono>
#include "seekdb_cuvs_bridge.h"
struct FakeCagraIndex { long n=0,dim=0;std::vector<float> data; };
static std::string fail_at;
static int fail_countdown=1, resources=0, indexes=0, index_params=0, search_params=0, ivfpq_params=0, builds=0, searches=0, allocations=0;
static size_t default_itopk=64;
static std::map<void*,size_t> live;
static std::atomic<int> active_searches{0},maximum_searches{0};
static bool fail(const char* name){if(fail_at==name && --fail_countdown==0){fail_at.clear();return true;}return false;}
static void inject(const char* name,int nth=1){fail_at=name;fail_countdown=nth;}
static void clean(){assert(resources==0&&indexes==0&&index_params==0&&search_params==0&&ivfpq_params==0&&live.empty());}
extern "C" {
const char* cuvsGetLastErrorText(){return "injected test failure";}
cuvsError_t cuvsResourcesCreate(cuvsResources_t* r){if(fail("resources"))return CUVS_ERROR;*r=malloc(1);++resources;return CUVS_SUCCESS;}
cuvsError_t cuvsResourcesDestroy(cuvsResources_t r){assert(r);free(r);--resources;return CUVS_SUCCESS;}
cuvsError_t cuvsStreamSync(cuvsResources_t){return fail("sync")?CUVS_ERROR:CUVS_SUCCESS;}
cuvsError_t cuvsRMMAlloc(cuvsResources_t,void** p,size_t n){if(fail("alloc"))return CUVS_ERROR;*p=malloc(n);assert(*p);live[*p]=n;++allocations;return CUVS_SUCCESS;}
cuvsError_t cuvsRMMFree(cuvsResources_t,void* p,size_t n){assert(live.count(p)&&live.at(p)==n);if(fail("free"))return CUVS_ERROR;live.erase(p);free(p);return CUVS_SUCCESS;}
cuvsError_t cuvsCagraIndexParamsCreate(cuvsCagraIndexParams_t* p){if(fail("ip"))return CUVS_ERROR;*p=(cuvsCagraIndexParams_t)calloc(1,sizeof(**p));(*p)->graph_build_params=(cuvsIvfPqParams_t)calloc(1,sizeof(*(*p)->graph_build_params));++index_params;return CUVS_SUCCESS;}
cuvsError_t cuvsCagraIndexParamsDestroy(cuvsCagraIndexParams_t p){free(p->graph_build_params);free(p);--index_params;return CUVS_SUCCESS;}
cuvsError_t cuvsIvfPqIndexParamsCreate(cuvsIvfPqIndexParams_t* p){assert(p);*p=(cuvsIvfPqIndexParams_t)calloc(1,sizeof(**p));++ivfpq_params;return CUVS_SUCCESS;}
cuvsError_t cuvsIvfPqIndexParamsDestroy(cuvsIvfPqIndexParams_t p){assert(p);free(p);--ivfpq_params;return CUVS_SUCCESS;}
cuvsError_t cuvsIvfPqSearchParamsCreate(cuvsIvfPqSearchParams_t* p){assert(p);*p=(cuvsIvfPqSearchParams_t)calloc(1,sizeof(**p));++ivfpq_params;return CUVS_SUCCESS;}
cuvsError_t cuvsIvfPqSearchParamsDestroy(cuvsIvfPqSearchParams_t p){assert(p);free(p);--ivfpq_params;return CUVS_SUCCESS;}
cuvsError_t cuvsCagraIndexCreate(cuvsCagraIndex_t* p){if(fail("index"))return CUVS_ERROR;*p=new FakeCagraIndex;++indexes;return CUVS_SUCCESS;}
cuvsError_t cuvsCagraIndexDestroy(cuvsCagraIndex_t p){delete p;--indexes;return CUVS_SUCCESS;}
cuvsError_t cuvsCagraSearchParamsCreate(cuvsCagraSearchParams_t* p){if(fail("sp"))return CUVS_ERROR;*p=(cuvsCagraSearchParams_t)calloc(1,sizeof(**p));(*p)->itopk_size=default_itopk;++search_params;return CUVS_SUCCESS;}
cuvsError_t cuvsCagraSearchParamsDestroy(cuvsCagraSearchParams_t p){free(p);--search_params;return CUVS_SUCCESS;}
cuvsError_t cuvsCagraBuild(cuvsResources_t,cuvsCagraIndexParams_t p,DLManagedTensor* d,cuvsCagraIndex_t i){
 assert(p->build_algo==NN_DESCENT&&p->graph_degree==160&&p->intermediate_graph_degree==192&&p->nn_descent_niter==40);
 assert(d->manager_ctx==nullptr&&d->deleter==nullptr&&d->dl_tensor.dtype.bits==32&&d->dl_tensor.device.device_type==kDLCPU);
 if(fail("build"))return CUVS_ERROR;i->n=d->dl_tensor.shape[0];i->dim=d->dl_tensor.shape[1];float* ptr=(float*)d->dl_tensor.data;i->data.assign(ptr,ptr+i->n*i->dim);++builds;return CUVS_SUCCESS;
}
cuvsError_t cuvsCagraSearch(cuvsResources_t,cuvsCagraSearchParams_t p,cuvsCagraIndex_t i,DLManagedTensor* q,DLManagedTensor* ids,DLManagedTensor* dst,cuvsFilter f){
 if(fail("search"))return CUVS_ERROR;
 const long nq=q->dl_tensor.shape[0],k=ids->dl_tensor.shape[1];
 assert(p->itopk_size==std::max(default_itopk,std::max(size_t(512),size_t(k*4)))&&p->search_width==4);
 assert(q->manager_ctx==nullptr&&q->deleter==nullptr&&ids->manager_ctx==nullptr&&dst->manager_ctx==nullptr);
 assert(q->dl_tensor.device.device_type==kDLCUDA&&f.type==NO_FILTER);
 int current=++active_searches;maximum_searches.store(std::max(maximum_searches.load(),current));std::this_thread::sleep_for(std::chrono::microseconds(50));
 for(long row=0;row<nq;++row){std::vector<std::pair<float,unsigned>> rank;
  for(long id=0;id<i->n;++id){float distance=0;for(long col=0;col<i->dim;++col){float diff=i->data[id*i->dim+col]-((float*)q->dl_tensor.data)[row*i->dim+col];distance+=diff*diff;}rank.emplace_back(distance,id);}
  std::sort(rank.begin(),rank.end());for(long pos=0;pos<k;++pos){((unsigned*)ids->dl_tensor.data)[row*k+pos]=rank[pos].second;((float*)dst->dl_tensor.data)[row*k+pos]=rank[pos].first;}
 }
 --active_searches;++searches;return CUVS_SUCCESS;
}
cudaError_t cudaMemcpy(void* dst,const void* src,size_t n,int){if(fail("copy"))return cudaErrorUnknown;memcpy(dst,src,n);return cudaSuccess;}
cuvsError_t cuvsCagraSerialize(cuvsResources_t,const char* path,cuvsCagraIndex_t i,bool include){assert(include);if(fail("serialize"))return CUVS_ERROR;std::ofstream f(path,std::ios::binary);f.write((char*)&i->n,sizeof(i->n));f.write((char*)&i->dim,sizeof(i->dim));f.write((char*)i->data.data(),i->data.size()*sizeof(float));return f?CUVS_SUCCESS:CUVS_ERROR;}
cuvsError_t cuvsCagraDeserialize(cuvsResources_t,const char* path,cuvsCagraIndex_t i){if(fail("deserialize"))return CUVS_ERROR;std::ifstream f(path,std::ios::binary);if(!f)return CUVS_ERROR;f.read((char*)&i->n,sizeof(i->n));f.read((char*)&i->dim,sizeof(i->dim));if(!f||i->n<=0||i->dim<=0)return CUVS_ERROR;i->data.resize(i->n*i->dim);f.read((char*)i->data.data(),i->data.size()*sizeof(float));return f?CUVS_SUCCESS:CUVS_ERROR;}
cuvsError_t cuvsCagraIndexGetSize(cuvsCagraIndex_t i,int64_t* n){if(fail("size"))return CUVS_ERROR;*n=i->n;return CUVS_SUCCESS;}
cuvsError_t cuvsCagraIndexGetDims(cuvsCagraIndex_t i,int64_t* n){if(fail("dims"))return CUVS_ERROR;*n=i->dim;return CUVS_SUCCESS;}
}
static std::vector<float> vectors(){std::vector<float> v(600*8);for(size_t i=0;i<v.size();i++)v[i]=float(i%997)/997;return v;}
static int query(void* h,int nq,int k,bool distance=true){auto v=vectors();std::vector<unsigned> ids(nq*k);std::vector<float> distances(nq*k);int rc=seekdb_cuvs_search(h,v.data(),nq,k,ids.data(),distance?distances.data():nullptr);if(!rc){for(int row=0;row<nq;++row){assert(ids[row*k]==unsigned(row));if(distance)assert(distances[row*k]==0);}}return rc;}
int main(){
 auto v=vectors();int checks=0;
 assert(!seekdb_cuvs_build(nullptr,600,8));assert(!seekdb_cuvs_build(v.data(),0,8));assert(!seekdb_cuvs_build(v.data(),-1,8));assert(!seekdb_cuvs_build(v.data(),600,std::numeric_limits<long>::max()));seekdb_cuvs_free(nullptr);clean();++checks;
 for(const char* point:{"resources","ip","index","sp","build","sync"}){inject(point);assert(!seekdb_cuvs_build(v.data(),600,8));clean();++checks;}
 void* h=seekdb_cuvs_build(v.data(),600,8);assert(h);assert(!query(h,1,10));int allocated=allocations;for(int n=0;n<20;++n)assert(!query(h,1,10));assert(allocations==allocated);++checks;
 for(auto shape:std::vector<std::pair<int,int>>{{4,100},{1,1},{2,129},{4,100},{1,10}})assert(!query(h,shape.first,shape.second));++checks;
 unsigned out[1];assert(seekdb_cuvs_search(h,v.data(),1,601,out,nullptr)!=0);assert(seekdb_cuvs_search(h,v.data(),0,1,out,nullptr)!=0);assert(seekdb_cuvs_search(h,v.data(),std::numeric_limits<long>::max(),1,out,nullptr)!=0);assert(seekdb_cuvs_search(h,v.data(),1,1,nullptr,nullptr)!=0);++checks;
 assert(!seekdb_cuvs_serialize(h,"snapshot.bin"));int builds_before=builds;seekdb_cuvs_free(h);clean();h=seekdb_cuvs_deserialize("snapshot.bin",600,8);assert(h&&builds==builds_before);assert(!query(h,4,100));++checks;
 std::vector<std::thread> threads;for(int t=0;t<4;++t)threads.emplace_back([&]{for(int i=0;i<5;++i)assert(!query(h,1,10));});for(auto& t:threads)t.join();assert(maximum_searches==1);++checks;
 for(int nth:{1,2,3}){inject("copy",nth);assert(query(h,1,10)!=0);assert(!query(h,1,10));++checks;}
 for(const char* point:{"search","sync"}){inject(point);assert(query(h,1,10)!=0);assert(!query(h,1,10));++checks;}
 assert(!query(h,1,10,false));inject("serialize");assert(seekdb_cuvs_serialize(h,"bad.bin")!=0);assert(!query(h,1,10));seekdb_cuvs_free(h);clean();++checks;
 for(const char* point:{"resources","ip","index","sp","deserialize","sync","size","dims"}){inject(point);assert(!seekdb_cuvs_deserialize("snapshot.bin",600,8));clean();++checks;}
 assert(!seekdb_cuvs_deserialize("snapshot.bin",599,8));assert(!seekdb_cuvs_deserialize("snapshot.bin",600,9));assert(!seekdb_cuvs_deserialize("missing.bin",600,8));assert(!seekdb_cuvs_deserialize(nullptr,600,8));clean();++checks;
 for(int nth:{1,2,3}){h=seekdb_cuvs_deserialize("snapshot.bin",600,8);assert(h);assert(!query(h,1,10));inject("alloc",nth);assert(query(h,4,100)!=0);assert(!query(h,1,10));assert(!query(h,4,100));seekdb_cuvs_free(h);clean();++checks;}
 h=seekdb_cuvs_deserialize("snapshot.bin",600,8);assert(h);assert(!query(h,1,10));inject("free");assert(query(h,4,100)!=0);assert(!query(h,1,10));assert(!query(h,4,100));seekdb_cuvs_free(h);clean();++checks;
 default_itopk=1024;h=seekdb_cuvs_deserialize("snapshot.bin",600,8);assert(h);assert(!query(h,4,100));assert(!query(h,1,10));seekdb_cuvs_free(h);clean();++checks;
 std::remove("snapshot.bin");std::printf("PASS %d host contract groups; validated 160/192/40 NN-descent build and unchanged V5 search parameters; snapshot reload without rebuild; scratch reuse/growth/recovery; 4 concurrent callers; no leaked resources. GPU NOT exercised.\n",checks);
}
