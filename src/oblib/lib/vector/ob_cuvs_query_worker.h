/* Copyright (c) 2026 OceanBase. Licensed under the Apache License, Version 2.0. */
#ifndef OCEANBASE_LIB_VECTOR_OB_CUVS_QUERY_WORKER_H_
#define OCEANBASE_LIB_VECTOR_OB_CUVS_QUERY_WORKER_H_
#include <pthread.h>
#include <condition_variable>
#include <mutex>

namespace oceanbase {
namespace common {
namespace obvsag {
// Synchronous dispatch preserves the caller's argument lifetime. CAGRA needs a
// large stack; keeping that stack/thread avoids per-query pthread and TLS setup.
class CuvsQueryWorker final {
public:
  using Job = void *(*)(void *);
  CuvsQueryWorker() = default;
  CuvsQueryWorker(const CuvsQueryWorker &) = delete;
  CuvsQueryWorker &operator=(const CuvsQueryWorker &) = delete;
  ~CuvsQueryWorker()
  {
    // The owner must prevent new calls during object destruction.
    std::lock_guard<std::mutex> submit(submit_mutex_);
    {
      std::lock_guard<std::mutex> state(state_mutex_);
      stop_ = true;
    }
    ready_.notify_one();
    if (started_) { pthread_join(thread_, nullptr); }
  }

  bool run(Job fn, void *arg)
  {
    if (fn == nullptr || current_worker() == this) { return false; }
    std::lock_guard<std::mutex> submit(submit_mutex_);
    std::unique_lock<std::mutex> state(state_mutex_);
    if (!started_) {
      pthread_attr_t attr;
      if (pthread_attr_init(&attr) != 0) { return false; }
      int rc = pthread_attr_setstacksize(&attr, 32UL * 1024 * 1024);
      if (rc == 0) { rc = pthread_create(&thread_, &attr, entry, this); }
      pthread_attr_destroy(&attr);
      if (rc != 0) { return false; }
      started_ = true;
    }
    fn_ = fn;
    arg_ = arg;
    pending_ = true;
    succeeded_ = false;
    ready_.notify_one();
    done_.wait(state, [this] { return !pending_; });
    return succeeded_;
  }
private:
  static CuvsQueryWorker *&current_worker()
  {
    static thread_local CuvsQueryWorker *worker = nullptr;
    return worker;
  }
  static void *entry(void *arg)
  {
    auto *worker = static_cast<CuvsQueryWorker *>(arg);
    current_worker() = worker;
    worker->loop();
    current_worker() = nullptr;
    return nullptr;
  }
  void loop()
  {
    std::unique_lock<std::mutex> state(state_mutex_);
    while (true) {
      ready_.wait(state, [this] { return stop_ || pending_; });
      if (stop_) { break; }
      Job fn = fn_;
      void *arg = arg_;
      state.unlock();
      bool ok = true;
      try { fn(arg); } catch (...) { ok = false; }
      state.lock();
      fn_ = nullptr;
      arg_ = nullptr;
      succeeded_ = ok;
      pending_ = false;
      done_.notify_one();
    }
  }
  pthread_t thread_{};
  std::mutex submit_mutex_;
  std::mutex state_mutex_;
  std::condition_variable ready_;
  std::condition_variable done_;
  Job fn_ = nullptr;
  void *arg_ = nullptr;
  bool started_ = false;
  bool stop_ = false;
  bool pending_ = false;
  bool succeeded_ = false;
};
} // namespace obvsag
} // namespace common
} // namespace oceanbase
#endif
