/*
 * Prints the size and alignment of every type in C's and POSIX's headers
 * whose layout a program compiled against one C library bakes in, and the
 * offset of each field a program reads or writes. Built once against
 * ferrousli's headers and once against glibc's, the two outputs differ
 * exactly where a program built against one would misread the other.
 * tools/abi-compare/compare.sh builds and compares them.
 *
 * glibc hides the GNU names of some fields; where a field's name differs
 * between the two libraries, only the type's size is printed.
 */

#define _GNU_SOURCE
#include <dirent.h>
#include <fcntl.h>
#include <fenv.h>
#include <glob.h>
#include <grp.h>
#include <ifaddrs.h>
#include <inttypes.h>
#include <link.h>
#include <locale.h>
#include <mntent.h>
#include <netdb.h>
#include <net/if.h>
#include <netinet/in.h>
#include <poll.h>
#include <pthread.h>
#include <pwd.h>
#include <regex.h>
#include <semaphore.h>
#include <setjmp.h>
#include <shadow.h>
#include <signal.h>
#include <spawn.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/epoll.h>
#include <sys/ipc.h>
#include <sys/msg.h>
#include <sys/resource.h>
#include <sys/select.h>
#include <sys/sem.h>
#include <sys/shm.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/statfs.h>
#include <sys/statvfs.h>
#include <sys/sysinfo.h>
#include <sys/time.h>
#include <sys/times.h>
#include <sys/uio.h>
#include <sys/un.h>
#include <sys/utsname.h>
#include <termios.h>
#include <time.h>
#include <utime.h>
#include <utmpx.h>
#include <wchar.h>
#include <getopt.h>
#include <sys/ioctl.h>
#include <dlfcn.h>

#define T(type) printf("%-28s size %3zu align %2zu\n", #type, sizeof(type), _Alignof(type))
#define F(type, field) printf("  %-26s %-16s %3zu\n", #type, #field, offsetof(type, field))

int main(void)
{
	/* Scalars. */
	T(char); T(short); T(int); T(long); T(long long); T(void *);
	T(float); T(double); T(long double); T(wchar_t); T(wint_t);
	T(size_t); T(ssize_t); T(ptrdiff_t); T(intmax_t); T(max_align_t);
	T(time_t); T(clock_t); T(suseconds_t); T(off_t); T(ino_t); T(dev_t);
	T(nlink_t); T(mode_t); T(uid_t); T(gid_t); T(pid_t); T(blksize_t);
	T(blkcnt_t); T(fsblkcnt_t); T(fsfilcnt_t); T(rlim_t); T(socklen_t);
	T(sig_atomic_t); T(clockid_t); T(key_t); T(id_t);

	/* Time. */
	T(struct timespec); F(struct timespec, tv_nsec);
	T(struct timeval); F(struct timeval, tv_usec);
	T(struct itimerval); F(struct itimerval, it_value);
	T(struct itimerspec); F(struct itimerspec, it_value);
	T(struct tm); F(struct tm, tm_isdst); F(struct tm, tm_gmtoff); F(struct tm, tm_zone);
	T(struct tms); F(struct tms, tms_stime); F(struct tms, tms_cstime);
	T(struct timezone);

	/* Files. */
	T(struct stat);
	F(struct stat, st_dev); F(struct stat, st_ino); F(struct stat, st_mode);
	F(struct stat, st_nlink); F(struct stat, st_uid); F(struct stat, st_gid);
	F(struct stat, st_rdev); F(struct stat, st_size); F(struct stat, st_blksize);
	F(struct stat, st_blocks); F(struct stat, st_atim); F(struct stat, st_mtim);
	F(struct stat, st_ctim);
	T(struct statfs); F(struct statfs, f_bsize); F(struct statfs, f_blocks);
	F(struct statfs, f_files); F(struct statfs, f_fsid); F(struct statfs, f_namelen);
	F(struct statfs, f_frsize); F(struct statfs, f_flags);
	T(struct statvfs); F(struct statvfs, f_frsize); F(struct statvfs, f_blocks);
	F(struct statvfs, f_files); F(struct statvfs, f_fsid); F(struct statvfs, f_flag);
	F(struct statvfs, f_namemax);
	T(struct dirent); F(struct dirent, d_off); F(struct dirent, d_reclen);
	F(struct dirent, d_type); F(struct dirent, d_name);
	T(struct flock); F(struct flock, l_start); F(struct flock, l_len); F(struct flock, l_pid);
	T(struct iovec); F(struct iovec, iov_len);
	T(struct pollfd); F(struct pollfd, revents);
	T(struct epoll_event); F(struct epoll_event, data);
	T(fd_set);
	T(struct utimbuf);
	T(glob_t); F(glob_t, gl_pathv); F(glob_t, gl_offs);
	T(struct mntent); F(struct mntent, mnt_freq); F(struct mntent, mnt_passno);

	/* Processes, resources and signals. */
	T(struct rlimit); F(struct rlimit, rlim_max);
	T(struct rusage); F(struct rusage, ru_stime); F(struct rusage, ru_maxrss);
	F(struct rusage, ru_nivcsw);
	T(sigset_t);
	T(struct sigaction); F(struct sigaction, sa_mask); F(struct sigaction, sa_flags);
	T(stack_t); F(stack_t, ss_flags); F(stack_t, ss_size);
	T(siginfo_t); F(siginfo_t, si_code); F(siginfo_t, si_pid); F(siginfo_t, si_uid);
	F(siginfo_t, si_status); F(siginfo_t, si_addr); F(siginfo_t, si_value);
	T(union sigval);
	T(jmp_buf); T(sigjmp_buf);
	T(struct utsname); F(struct utsname, machine);
	T(struct sysinfo); F(struct sysinfo, totalram); F(struct sysinfo, procs);
	F(struct sysinfo, totalhigh); F(struct sysinfo, mem_unit);
	T(posix_spawnattr_t); T(posix_spawn_file_actions_t);
	T(fenv_t); T(fexcept_t);

	/* Threads. */
	T(pthread_t); T(pthread_attr_t); T(pthread_mutex_t); T(pthread_mutexattr_t);
	T(pthread_cond_t); T(pthread_condattr_t); T(pthread_rwlock_t);
	T(pthread_rwlockattr_t); T(pthread_barrier_t); T(pthread_barrierattr_t);
	T(pthread_spinlock_t); T(pthread_key_t); T(pthread_once_t); T(sem_t);

	/* Users. */
	T(struct passwd); F(struct passwd, pw_uid); F(struct passwd, pw_dir); F(struct passwd, pw_shell);
	T(struct group); F(struct group, gr_mem);
	T(struct spwd); F(struct spwd, sp_lstchg); F(struct spwd, sp_flag);
	T(struct utmpx);

	/* Sockets and names. */
	T(struct sockaddr); T(struct sockaddr_storage); T(struct sockaddr_in);
	T(struct sockaddr_in6); T(struct sockaddr_un);
	T(struct msghdr); F(struct msghdr, msg_iov); F(struct msghdr, msg_iovlen);
	F(struct msghdr, msg_control); F(struct msghdr, msg_controllen); F(struct msghdr, msg_flags);
	T(struct cmsghdr); F(struct cmsghdr, cmsg_level);
	T(struct linger);
	T(struct addrinfo); F(struct addrinfo, ai_addrlen); F(struct addrinfo, ai_addr);
	F(struct addrinfo, ai_canonname); F(struct addrinfo, ai_next);
	T(struct hostent); F(struct hostent, h_addr_list);
	T(struct servent); F(struct servent, s_proto);
	T(struct protoent); F(struct protoent, p_proto);
	T(struct netent); F(struct netent, n_net);
	T(struct ifaddrs); F(struct ifaddrs, ifa_addr); F(struct ifaddrs, ifa_data);
	T(struct if_nameindex);

	/* IPC. */
	T(struct ipc_perm); F(struct ipc_perm, mode);
	T(struct semid_ds); F(struct semid_ds, sem_otime); F(struct semid_ds, sem_ctime);
	F(struct semid_ds, sem_nsems);
	T(struct shmid_ds); F(struct shmid_ds, shm_segsz); F(struct shmid_ds, shm_atime);
	F(struct shmid_ds, shm_ctime); F(struct shmid_ds, shm_cpid); F(struct shmid_ds, shm_nattch);
	T(struct msqid_ds); F(struct msqid_ds, msg_stime); F(struct msqid_ds, msg_ctime);
	F(struct msqid_ds, msg_qnum); F(struct msqid_ds, msg_lspid);

	/* Terminals. */
	T(struct termios); F(struct termios, c_cc); F(struct termios, c_line);
	T(struct winsize);

	/* The rest. */
	T(struct lconv); F(struct lconv, int_n_sign_posn);
	T(regex_t); F(regex_t, re_nsub); T(regmatch_t);
	T(struct option); F(struct option, val);
	T(mbstate_t); T(div_t); T(ldiv_t); T(lldiv_t); T(imaxdiv_t);
	T(struct dl_phdr_info); F(struct dl_phdr_info, dlpi_phdr); F(struct dl_phdr_info, dlpi_phnum);
	T(Dl_info); F(Dl_info, dli_saddr);
	T(cookie_io_functions_t);
	T(fpos_t);
	return 0;
}
