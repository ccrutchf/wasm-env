// dlopen and dlclose of the same library many times over, its constructor
// run and a function called each time (as SDL does libudev's, polling for
// devices): each load the same library, not a new one, so nothing fills up.
// Meanwhile another thread opens and closes a file over and over, as a
// program's other threads do: the library still lands in its own range.
#include <dlfcn.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <unistd.h>

static atomic_int done;

static void *opener(void *arg) {
    while (!atomic_load(&done)) {
        int fd = open("/etc/os-release", O_RDONLY);
        if (fd >= 0)
            close(fd);
    }
    return arg;
}

int main(void) {
    pthread_t t;
    pthread_create(&t, NULL, opener, NULL);
    long sum = 0;
    for (int i = 0; i < 5000; i++) {
        void *h = dlopen("/usr/lib/libplugin.so", RTLD_NOW);
        if (!h) {
            printf("dlopen %d: %s\n", i, dlerror());
            return 1;
        }
        int (*value)(int) = (int (*)(int))dlsym(h, "plugin_value");
        sum += value(i);
        dlclose(h);
    }
    atomic_store(&done, 1);
    pthread_join(t, NULL);
    printf("5000 loads, sum %ld\n", sum);
    return 0;
}
