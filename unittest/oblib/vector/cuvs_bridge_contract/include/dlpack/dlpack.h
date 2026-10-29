#pragma once
#include <stdint.h>
typedef enum { kDLCPU=1, kDLCUDA=2 } DLDeviceType;
typedef struct { DLDeviceType device_type; int device_id; } DLDevice;
typedef struct { uint8_t code,bits; uint16_t lanes; } DLDataType;
enum { kDLUInt=1, kDLFloat=2 };
typedef struct { void *data; DLDevice device; int ndim; DLDataType dtype; int64_t *shape,*strides; uint64_t byte_offset; } DLTensor;
typedef struct DLManagedTensor { DLTensor dl_tensor; void *manager_ctx; void (*deleter)(struct DLManagedTensor *); } DLManagedTensor;
