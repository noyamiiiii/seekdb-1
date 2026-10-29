#pragma once
#include <stddef.h>
#include <stdint.h>
#include <stdbool.h>
#ifdef __cplusplus
extern "C" {
#endif
typedef void* cuvsResources_t;
typedef enum {CUVS_SUCCESS=0, CUVS_ERROR=1} cuvsError_t;
cuvsError_t cuvsResourcesCreate(cuvsResources_t*);
cuvsError_t cuvsResourcesDestroy(cuvsResources_t);
cuvsError_t cuvsStreamSync(cuvsResources_t);
cuvsError_t cuvsRMMAlloc(cuvsResources_t,void**,size_t);
cuvsError_t cuvsRMMFree(cuvsResources_t,void*,size_t);
const char* cuvsGetLastErrorText(void);
#ifdef __cplusplus
}
#endif
