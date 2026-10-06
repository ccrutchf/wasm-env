// dlopen and dlclose of the same library many times over, its constructor
// run and a function called each time (as SDL does libudev's, polling for
// devices): each load the same library, not a new one, so nothing fills up.
#include <dlfcn.h>
#include <stdio.h>

int main(void) {
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
    printf("5000 loads, sum %ld\n", sum);
    return 0;
}
