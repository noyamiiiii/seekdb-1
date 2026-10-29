#pragma once
#include <stddef.h>
#ifdef __cplusplus
extern "C" {
#endif
typedef enum {cudaSuccess=0,cudaErrorUnknown=1} cudaError_t;
enum {cudaMemcpyDefault=0};
cudaError_t cudaMemcpy(void*,const void*,size_t,int);
#ifdef __cplusplus
}
#endif
