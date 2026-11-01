/*
 * Private Darwin SDK boundary for in-process native filesystem credentials.
 * sysctl(KERN_PROC_PID) returns the actual target e_ucred group vector. Keep
 * kinfo_proc layout in the SDK-compiled translation unit, never guessed in Rust.
 * A missing, partial or oversized record fails closed without truncation or
 * substitution of calling-process groups. No helper process is executed.
 * Public definitions: Apple XNU bsd/sys/sysctl.h and kern/kern_sysctl.c.
 */
#include <errno.h>
#include <stdint.h>
#include <stddef.h>
#include <string.h>
#include <sys/types.h>
#include <sys/sysctl.h>

struct mez_native_credentials {
    uint32_t user_id;
    uint32_t primary_group_id;
    uint32_t group_count;
    uint32_t groups[64];
};
_Static_assert(sizeof(uid_t) == sizeof(uint32_t), "Darwin uid_t ABI changed");
_Static_assert(sizeof(gid_t) == sizeof(uint32_t), "Darwin gid_t ABI changed");
_Static_assert(sizeof(struct mez_native_credentials) == 67 * sizeof(uint32_t), "private credential ABI changed");

/* Initialize a complete private output record only after an exact target query. */
int mez_darwin_native_credentials(pid_t pid, struct mez_native_credentials *output) {
    if (pid <= 0 || output == NULL) { errno = EINVAL; return -1; }
    struct kinfo_proc info;
    memset(&info, 0, sizeof(info));
    int name[4] = { CTL_KERN, KERN_PROC, KERN_PROC_PID, pid };
    size_t size = sizeof(info);
    if (sysctl(name, 4, &info, &size, NULL, 0) != 0) { return -1; }
    if (size != sizeof(info) || info.kp_proc.p_pid != pid) { errno = ESRCH; return -1; }
    const int count = info.kp_eproc.e_ucred.cr_ngroups;
    const size_t capacity = sizeof(info.kp_eproc.e_ucred.cr_groups) / sizeof(gid_t);
    if (count <= 0 || (size_t)count > capacity || (size_t)count > 64) { errno = EOVERFLOW; return -1; }
    struct mez_native_credentials result;
    memset(&result, 0, sizeof(result));
    result.user_id = info.kp_eproc.e_ucred.cr_uid;
    result.primary_group_id = info.kp_eproc.e_ucred.cr_groups[0];
    result.group_count = (uint32_t)count;
    memcpy(result.groups, info.kp_eproc.e_ucred.cr_groups, (size_t)count * sizeof(gid_t));
    *output = result;
    return 0;
}
