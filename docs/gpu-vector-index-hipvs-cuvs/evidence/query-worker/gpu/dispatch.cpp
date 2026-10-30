#include "ob_cuvs_query_worker.h"
#include "seekdb_cuvs_bridge.h"
#include <pthread.h>
using oceanbase::common::obvsag::CuvsQueryWorker;
struct Job {int op;void *h;const float *base;long n,dim,k;unsigned *ids;float *dist;const char *path;int rc;};
static void *execute(void *p){auto *j=static_cast<Job*>(p);switch(j->op){
 case 0:j->h=seekdb_cuvs_build(j->base,j->n,j->dim);j->rc=j->h?0:1;break;
 case 1:j->rc=seekdb_cuvs_search(j->h,j->base,1,j->k,j->ids,j->dist);break;
 case 2:j->rc=seekdb_cuvs_serialize(j->h,j->path);break;
 case 3:j->h=seekdb_cuvs_deserialize(j->path,j->n,j->dim);j->rc=j->h?0:1;break;
 case 4:seekdb_cuvs_free(j->h);j->rc=0;break;}return nullptr;}
static int dispatch(Job &j,bool reuse){
 if(reuse){static CuvsQueryWorker w;if(!w.run(execute,&j))return 90;}
 else{pthread_t t;pthread_attr_t a;if(pthread_attr_init(&a))return 91;
 int rc=pthread_attr_setstacksize(&a,32UL*1024*1024);if(!rc)rc=pthread_create(&t,&a,execute,&j);pthread_attr_destroy(&a);if(rc)return 92;if(pthread_join(t,nullptr))return 93;}
 return j.rc;
}
extern "C" void *test_build(const float *base,long n,long dim){Job j{};j.op=0;j.base=base;j.n=n;j.dim=dim;return dispatch(j,false)?nullptr:j.h;}
extern "C" int test_query(void *h,const float*q,long k,unsigned *ids,float*dist,int reuse){Job j{};j.op=1;j.h=h;j.base=q;j.k=k;j.ids=ids;j.dist=dist;return dispatch(j,reuse);}
extern "C" int test_save(void*h,const char*path){Job j{};j.op=2;j.h=h;j.path=path;return dispatch(j,false);}
extern "C" void *test_load(const char*path,long n,long dim){Job j{};j.op=3;j.path=path;j.n=n;j.dim=dim;return dispatch(j,false)?nullptr:j.h;}
extern "C" int test_free(void*h){Job j{};j.op=4;j.h=h;return dispatch(j,false);}
