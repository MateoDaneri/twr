/*
 * Isolated observation of SCM_RIGHTS truncation. Run as its own process:
 *   cc -Wall -Wextra -Werror -o /private/tmp/twr-cmsg-probe \
 *     Resources/Fixtures/macos-truncated-scm-rights.c
 *   /private/tmp/twr-cmsg-probe
 * Uses only a private socketpair and pipe. No terminal or user session is used.
 * The scan is intentionally restricted to this short-lived synthetic process;
 * enumerating/closing unknown descriptors is not safe in a real application.
 */
#include <sys/socket.h>
#include <unistd.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <stdlib.h>

#define SCAN_LIMIT 1024
#define SENT 8

int main(void) {
    int sockets[2], pipefd[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sockets) || pipe(pipefd)) {
        perror("private endpoints");
        return 1;
    }
    union { struct cmsghdr align; unsigned char bytes[CMSG_SPACE(SENT * sizeof(int))]; } tx = {0};
    char payload = 'T';
    struct iovec iov = {&payload, 1};
    struct msghdr msg = {0};
    msg.msg_iov = &iov;
    msg.msg_iovlen = 1;
    msg.msg_control = tx.bytes;
    msg.msg_controllen = sizeof(tx.bytes);
    struct cmsghdr *cmsg = CMSG_FIRSTHDR(&msg);
    cmsg->cmsg_level = SOL_SOCKET;
    cmsg->cmsg_type = SCM_RIGHTS;
    cmsg->cmsg_len = CMSG_LEN(SENT * sizeof(int));
    for (int i = 0; i < SENT; ++i)
        memcpy(CMSG_DATA(cmsg) + i * sizeof(int), &pipefd[1], sizeof(int));
    if (sendmsg(sockets[0], &msg, 0) != 1) { perror("sendmsg"); return 1; }

    unsigned char before[SCAN_LIMIT];
    for (int fd = 0; fd < SCAN_LIMIT; ++fd) before[fd] = fcntl(fd, F_GETFD) != -1;
    union { struct cmsghdr align; unsigned char bytes[CMSG_SPACE(2 * sizeof(int))]; } rx;
    memset(&rx, 0, sizeof(rx));
    memset(&msg, 0, sizeof(msg));
    msg.msg_iov = &iov;
    msg.msg_iovlen = 1;
    msg.msg_control = rx.bytes;
    msg.msg_controllen = sizeof(rx.bytes);
    if (recvmsg(sockets[1], &msg, 0) != 1) { perror("recvmsg"); return 1; }
    cmsg = (struct cmsghdr *)rx.bytes;
    size_t visible = msg.msg_controllen < sizeof(rx.bytes) ? msg.msg_controllen : sizeof(rx.bytes);
    size_t base = CMSG_LEN(0);
    size_t disclosed = visible >= base ? (visible - base) / sizeof(int) : 0;
    int new_fds = 0;
    for (int fd = 0; fd < SCAN_LIMIT; ++fd)
        if (!before[fd] && fcntl(fd, F_GETFD) != -1) ++new_fds;
    printf("sent=%d control_capacity=%zu returned=%u cmsg_len=%u truncated=%d disclosed_slots=%zu new_process_fds=%d\n",
        SENT, sizeof(rx.bytes), (unsigned)msg.msg_controllen, (unsigned)cmsg->cmsg_len,
        !!(msg.msg_flags & MSG_CTRUNC), disclosed, new_fds);
    /* The kernel closes every synthetic endpoint when this process exits. */
    return !(msg.msg_flags & MSG_CTRUNC);
}
