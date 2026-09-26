/*
 * System V IPC: a private shared memory segment seen through two
 * attachments, a semaphore set, and a message queue, each removed at the end.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <string.h>
#include <sys/ipc.h>
#include <sys/msg.h>
#include <sys/sem.h>
#include <sys/shm.h>
#include <time.h>

#include "check.h"

/* The caller defines it, as POSIX says. */
union semun {
	int val;
	struct semid_ds *buf;
	unsigned short *array;
};

int main(void)
{
	int shm, sem, queue;
	char *a, *b;
	struct shmid_ds shm_ds;
	struct semid_ds sem_ds;
	struct msqid_ds msg_ds;
	union semun arg;
	unsigned short values[2];
	struct sembuf op;
	struct timespec wait = { 0, 1000000 };
	struct {
		long type;
		char text[8];
	} out = { 7, "hello" }, in;

	/* Shared memory, written through one attachment and read through a
	 * read-only one. */
	shm = shmget(IPC_PRIVATE, 8192, IPC_CREAT | 0600);
	CHECK(shm >= 0);
	a = shmat(shm, 0, 0);
	b = shmat(shm, 0, SHM_RDONLY);
	CHECK(a != (void *)-1 && b != (void *)-1 && a != b);
	if (a != (void *)-1 && b != (void *)-1) {
		strcpy(a, "shared");
		CHECK(!strcmp(b, "shared"));
	}
	CHECK(shmctl(shm, IPC_STAT, &shm_ds) == 0);
	CHECK(shm_ds.shm_segsz == 8192 && shm_ds.shm_nattch == 2);
	CHECK(shmdt(b) == 0 && shmdt(a) == 0);
	CHECK(shmctl(shm, IPC_RMID, 0) == 0);
	errno = 0;
	CHECK(shmat(shm, 0, 0) == (void *)-1 && (errno == EINVAL || errno == EIDRM));

	/* Semaphores: a value set, taken from, refused past zero, and read back
	 * whole. */
	sem = semget(IPC_PRIVATE, 2, IPC_CREAT | 0600);
	CHECK(sem >= 0);
	arg.val = 3;
	CHECK(semctl(sem, 1, SETVAL, arg) == 0);
	CHECK(semctl(sem, 1, GETVAL) == 3);
	op.sem_num = 1;
	op.sem_op = -2;
	op.sem_flg = IPC_NOWAIT;
	CHECK(semop(sem, &op, 1) == 0 && semctl(sem, 1, GETVAL) == 1);
	errno = 0;
	CHECK(semtimedop(sem, &op, 1, &wait) == -1 && errno == EAGAIN);
	arg.array = values;
	CHECK(semctl(sem, 0, GETALL, arg) == 0 && values[0] == 0 && values[1] == 1);
	arg.buf = &sem_ds;
	CHECK(semctl(sem, 0, IPC_STAT, arg) == 0 && sem_ds.sem_nsems == 2);
	CHECK(semctl(sem, 0, IPC_RMID) == 0);
	errno = 0;
	CHECK(semget(IPC_PRIVATE, 70000, IPC_CREAT | 0600) == -1 && errno == EINVAL);

	/* A message queue: one message sent, counted, received by type. */
	queue = msgget(IPC_PRIVATE, IPC_CREAT | 0600);
	CHECK(queue >= 0);
	CHECK(msgsnd(queue, &out, sizeof out.text, 0) == 0);
	CHECK(msgctl(queue, IPC_STAT, &msg_ds) == 0 && msg_ds.msg_qnum == 1);
	CHECK(msgrcv(queue, &in, sizeof in.text, 7, 0) == (ssize_t)sizeof in.text);
	CHECK(in.type == 7 && !strcmp(in.text, "hello"));
	errno = 0;
	CHECK(msgrcv(queue, &in, sizeof in.text, 0, IPC_NOWAIT) == -1 && errno == ENOMSG);
	CHECK(msgctl(queue, IPC_RMID, 0) == 0);
	return t_status;
}
