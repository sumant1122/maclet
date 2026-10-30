#ifndef APPLE_VZ_H
#define APPLE_VZ_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct VzVm VzVm;

typedef struct {
    uint32_t cpu_count;
    uint64_t memory_bytes;
    const char *kernel_path;
    const char *initrd_path;
    const char *cmdline;
    const char *disk_path;
    uint8_t enable_nat_network;
} VzConfig;

int32_t vz_is_supported(void);

int32_t vz_create_vm(
    const VzConfig *config,
    int32_t console_write_fd,
    int32_t ipc_socket_fd,
    VzVm **out_vm,
    char *err_buf,
    size_t err_len
);

int32_t vz_start_vm(
    VzVm *vm,
    char *err_buf,
    size_t err_len
);

int32_t vz_stop_vm(
    VzVm *vm,
    char *err_buf,
    size_t err_len
);

void vz_destroy_vm(VzVm *vm);

#ifdef __cplusplus
}
#endif

#endif // APPLE_VZ_H
