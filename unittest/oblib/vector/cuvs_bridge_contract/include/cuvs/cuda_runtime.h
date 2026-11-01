#pragma once
#include <stddef.h>
#ifdef __cplusplus
extern "C" {
#endif
typedef enum {cudaSuccess=0,cudaErrorUnknown=1} cudaError_t;
enum {cudaMemcpyDefault=0};
cudaError_t cudaMemcpy(void*,const void*,size_t,int);
cudaError_t cudaMemcpy2D(void*,size_t,const void*,size_t,size_t,size_t,int);
#define hipMemcpy2D cudaMemcpy2D
#define hipMemcpyDefault cudaMemcpyDefault
#define hipSuccess cudaSuccess
#ifdef __cplusplus
}
#endif
