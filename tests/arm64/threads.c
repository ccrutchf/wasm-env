// Threads: creation and join, thread-local storage, atomics (exclusives
// by default; LSE with -march=armv8.1-a), mutexes, condition variables,
// barriers, semaphores and pthread_once, all contended by threads running
// at once. The output doesn't depend on scheduling.
#define _GNU_SOURCE
#include <pthread.h>
#include <semaphore.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

#define THREADS 8
#define ROUNDS 20000

static atomic_long counter;
static atomic_uint_least8_t small; // byte-wide atomics
static long locked_counter;
static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static atomic_flag spin = ATOMIC_FLAG_INIT;
static long spun_counter;
static volatile int go_again; // never set: the spin below must time out
static __thread long mine = 1;
static __thread char name[16];

static void *work(void *arg)
{
	long id = (long)arg;
	mine = id * 1000;
	snprintf(name, sizeof name, "thread %ld", id);
	for (int i = 0; i < ROUNDS; i++) {
		atomic_fetch_add(&counter, 1);
		atomic_fetch_add_explicit(&small, 1, memory_order_relaxed);
		pthread_mutex_lock(&lock);
		locked_counter++;
		pthread_mutex_unlock(&lock);
		while (atomic_flag_test_and_set_explicit(&spin, memory_order_acquire))
			;
		spun_counter++;
		atomic_flag_clear_explicit(&spin, memory_order_release);
		mine++;
	}
	// Each thread's own copy survived the others'.
	if (mine != id * 1000 + ROUNDS) {
		printf("%s: thread-local variable is %ld\n", name, mine);
		exit(1);
	}
	return (void *)(id * id);
}

// A bounded queue: producers and consumers on a mutex and two conditions.
#define QUEUE 4
#define ITEMS 1000
static int queue[QUEUE], head, count;
static pthread_mutex_t qlock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t not_full = PTHREAD_COND_INITIALIZER, not_empty = PTHREAD_COND_INITIALIZER;

static void *produce(void *arg)
{
	(void)arg;
	for (int i = 1; i <= ITEMS; i++) {
		pthread_mutex_lock(&qlock);
		while (count == QUEUE)
			pthread_cond_wait(&not_full, &qlock);
		queue[(head + count++) % QUEUE] = i;
		pthread_cond_signal(&not_empty);
		pthread_mutex_unlock(&qlock);
	}
	return NULL;
}

static void *consume(void *arg)
{
	(void)arg;
	long sum = 0;
	for (int i = 0; i < ITEMS; i++) {
		pthread_mutex_lock(&qlock);
		while (count == 0)
			pthread_cond_wait(&not_empty, &qlock);
		sum += queue[head];
		head = (head + 1) % QUEUE;
		count--;
		pthread_cond_signal(&not_full);
		pthread_mutex_unlock(&qlock);
	}
	return (void *)sum;
}

static pthread_barrier_t barrier;
static atomic_int arrived;
static int after_barrier_ok = 1;

static void *meet(void *arg)
{
	(void)arg;
	atomic_fetch_add(&arrived, 1);
	pthread_barrier_wait(&barrier);
	if (atomic_load(&arrived) != THREADS)
		after_barrier_ok = 0;
	return NULL;
}

static pthread_once_t once = PTHREAD_ONCE_INIT;
static atomic_int once_runs;
static void init_once(void) { atomic_fetch_add(&once_runs, 1); }

static sem_t sem;
static atomic_int in_section, max_in_section;

static void *limited(void *arg)
{
	(void)arg;
	pthread_once(&once, init_once);
	for (int i = 0; i < 200; i++) {
		sem_wait(&sem);
		int n = atomic_fetch_add(&in_section, 1) + 1;
		int m = atomic_load(&max_in_section);
		while (n > m && !atomic_compare_exchange_weak(&max_in_section, &m, n))
			;
		atomic_fetch_sub(&in_section, 1);
		sem_post(&sem);
	}
	return NULL;
}

// A flag another thread sets, polled with plain loads (volatile, no
// barrier): the translation mustn't keep the first value it read.
static volatile int go;

static void *set_go(void *arg)
{
	(void)arg;
	usleep(20000);
	go = 1;
	return NULL;
}

int main(void)
{
	pthread_t t[THREADS];
	mine = 7;
	for (long i = 0; i < THREADS; i++)
		if (pthread_create(&t[i], NULL, work, (void *)i)) {
			puts("pthread_create failed");
			return 1;
		}
	long joined = 0;
	for (int i = 0; i < THREADS; i++) {
		void *r;
		pthread_join(t[i], &r);
		joined += (long)r;
	}
	printf("atomic counter: %ld\n", atomic_load(&counter));
	printf("byte counter: %d\n", atomic_load(&small));
	printf("mutex counter: %ld\n", locked_counter);
	printf("spinlock counter: %ld\n", spun_counter);
	printf("join results: %ld\n", joined);
	printf("main's thread-local: %ld\n", mine);

	pthread_t p, c;
	pthread_create(&c, NULL, consume, NULL);
	pthread_create(&p, NULL, produce, NULL);
	void *sum;
	pthread_join(p, NULL);
	pthread_join(c, &sum);
	printf("queue: consumed %ld\n", (long)sum);

	pthread_barrier_init(&barrier, NULL, THREADS);
	for (int i = 0; i < THREADS; i++)
		pthread_create(&t[i], NULL, meet, NULL);
	for (int i = 0; i < THREADS; i++)
		pthread_join(t[i], NULL);
	printf("barrier: %s\n", after_barrier_ok ? "everyone arrived first" : "broken");

	sem_init(&sem, 0, 3);
	for (int i = 0; i < THREADS; i++)
		pthread_create(&t[i], NULL, limited, NULL);
	for (int i = 0; i < THREADS; i++)
		pthread_join(t[i], NULL);
	printf("semaphore: at most %s in the section\n", atomic_load(&max_in_section) <= 3 ? "3" : "too many");
	printf("pthread_once: ran %d time(s)\n", atomic_load(&once_runs));

	pthread_t g;
	pthread_create(&g, NULL, set_go, NULL);
	while (!go)
		;
	pthread_join(g, NULL);
	puts("volatile flag: seen");

	// A spin loop with a timeout on the virtual counter (OpenBLAS's idle
	// threads wait like this): the counter must advance at its frequency.
	uint64_t freq, start, now;
	asm volatile("mrs %0, cntfrq_el0" : "=r"(freq));
	asm volatile("isb; mrs %0, cntvct_el0" : "=r"(start));
	do
		asm volatile("isb; mrs %0, cntvct_el0" : "=r"(now));
	while (!go_again && now - start < freq / 50);
	printf("counter timeout: %s\n", freq && !go_again ? "reached" : "broken");

	// A detached thread still running when main returns doesn't hold up exit.
	pthread_t d;
	pthread_create(&d, NULL, (void *(*)(void *))sleep, (void *)100);
	pthread_detach(d);
	printf("pid == tid of main: %s\n", getpid() == gettid() ? "yes" : "no");
	return 0;
}
