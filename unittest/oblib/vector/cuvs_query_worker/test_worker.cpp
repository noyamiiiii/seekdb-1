#include "ob_cuvs_query_worker.h"
#include <atomic>
#include <cassert>
#include <chrono>
#include <cstdio>
#include <stdexcept>
#include <thread>
#include <vector>
#include <cerrno>
using oceanbase::common::obvsag::CuvsQueryWorker;
static std::atomic<int> creates{0}, joins{0};
static std::atomic<bool> fail_create{false};
extern "C" int __real_pthread_create(pthread_t *, const pthread_attr_t *, void *(*)(void *), void *);
extern "C" int __wrap_pthread_create(pthread_t *t, const pthread_attr_t *a, void *(*fn)(void *), void *p) {
  if (fail_create.exchange(false)) return EAGAIN;
  int rc=__real_pthread_create(t,a,fn,p); if (!rc) ++creates; return rc;
}
extern "C" int __real_pthread_join(pthread_t, void **);
extern "C" int __wrap_pthread_join(pthread_t t, void **p) {
  int rc=__real_pthread_join(t,p); if (!rc) ++joins; return rc;
}
struct Result { int value=0; pthread_t thread{}; size_t stack=0; };
static void *write_result(void *p) {
  auto *r=static_cast<Result *>(p); ++r->value; r->thread=pthread_self();
  pthread_attr_t attr; assert(pthread_getattr_np(pthread_self(),&attr)==0);
  void *base=nullptr; assert(pthread_attr_getstack(&attr,&base,&r->stack)==0);
  pthread_attr_destroy(&attr); return nullptr;
}
static void *throws(void *) { throw std::runtime_error("injected callback error"); }
struct Recursive { CuvsQueryWorker *worker; bool rejected=false; };
static void *recursive(void *p) {
  auto *r=static_cast<Recursive *>(p); r->rejected=!r->worker->run(write_result,nullptr); return nullptr;
}
static std::atomic<int> active{0},maximum{0};
static void *isolated(void *p) {
  int count=++active; int old=maximum; while(old<count&&!maximum.compare_exchange_weak(old,count)){}
  ++*static_cast<int *>(p); std::this_thread::yield(); --active; return nullptr;
}
static void *touch(void *p) { ++*static_cast<int *>(p); return nullptr; }
static void tests() {
  { CuvsQueryWorker unused; assert(!unused.run(nullptr,nullptr)); }
  assert(creates==0&&joins==0);
  {
    CuvsQueryWorker w; Result r;
    fail_create=true; assert(!w.run(write_result,&r)); assert(r.value==0);
    assert(w.run(write_result,&r)); auto id=r.thread;
    assert(!pthread_equal(id,pthread_self())); assert(r.stack>=32UL*1024*1024);
    for(int i=0;i<100;i++){assert(w.run(write_result,&r));assert(pthread_equal(id,r.thread));}
    assert(r.value==101&&creates==1);
    assert(!w.run(throws,nullptr)); assert(w.run(write_result,&r)); assert(r.value==102);
    Recursive rr{&w}; assert(w.run(recursive,&rr)); assert(rr.rejected);
    std::vector<std::thread> callers; std::vector<int> counters(8,0);
    for(int i=0;i<8;i++)callers.emplace_back([&,i]{for(int j=0;j<1000;j++)assert(w.run(isolated,&counters[i]));});
    for(auto &t:callers)t.join(); for(int count:counters)assert(count==1000);
    assert(maximum==1&&active==0);
  }
  assert(joins==creates);
  for(int i=0;i<30;i++){CuvsQueryWorker w; int count=0;assert(w.run(touch,&count));assert(count==1);}
  assert(joins==creates);
  puts("PASS: lazy startup, null rejection, create failure/retry, 32 MiB stack, thread reuse, synchronous visibility, exception recovery, recursion rejection, 8 concurrent callers / 8000 jobs, serialized callbacks, shutdown and repeated lifecycle");
}
static double benchmark(bool reuse,int n) {
  int count=0; CuvsQueryWorker worker;
  pthread_attr_t attr;assert(pthread_attr_init(&attr)==0);assert(pthread_attr_setstacksize(&attr,32UL*1024*1024)==0);
  if(reuse)assert(worker.run(touch,&count)); count=0;
  auto start=std::chrono::steady_clock::now();
  for(int i=0;i<n;i++) {
    if(reuse)assert(worker.run(touch,&count));
    else {pthread_t t;assert(pthread_create(&t,&attr,touch,&count)==0);assert(pthread_join(t,nullptr)==0);}
  }
  double us=std::chrono::duration<double,std::micro>(std::chrono::steady_clock::now()-start).count()/n;
  pthread_attr_destroy(&attr);assert(count==n);return us;
}
int main(int argc,char **) {
  if(argc==1){tests();return 0;}
  for(int round=0;round<7;round++){
    double old_us,new_us;
    if(round%2){new_us=benchmark(true,3000);old_us=benchmark(false,3000);}
    else {old_us=benchmark(false,3000);new_us=benchmark(true,3000);}
    printf("{\"round\":%d,\"create_join_us\":%.6f,\"worker_us\":%.6f,\"host_dispatch_speedup\":%.6f}\n",round,old_us,new_us,old_us/new_us);
  }
}
