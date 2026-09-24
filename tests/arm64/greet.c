// A dynamically-linked program: libc, libgreet.so (a DT_NEEDED library),
// and a plugin loaded with dlopen. Exercises the translated dynamic linker
// mapping libraries at run time.
#include <dlfcn.h>
#include <errno.h>
#include <stdio.h>
#include <string.h>

extern int greet_ready;
const char *greet(const char *);
size_t greet_len(const char *);

int main(int argc, char **argv)
{
	printf("greet_ready=%d\n", greet_ready);
	printf("%s\n", greet("world"));
	printf("%s len=%zu\n", greet(argc > 1 ? argv[1] : "nobody"), greet_len("abcdef"));

	void *h = dlopen("libplugin.so", RTLD_NOW);
	if (!h) {
		printf("dlopen: %s\n", dlerror());
		return 1;
	}
	int (*value)(int) = (int (*)(int))dlsym(h, "plugin_value");
	const char *(*pg)(void) = (const char *(*)(void))dlsym(h, "plugin_greet");
	printf("plugin_value(5)=%d\n", value ? value(5) : -1);
	printf("%s\n", pg ? pg() : "(no plugin_greet)");
	printf("missing symbol: %s\n", dlsym(h, "nope") ? "found?" : "not found");
	errno = 0;
	printf("dlopen missing: %s\n", dlopen("libnope.so", RTLD_NOW) ? "loaded?" : "failed");
	return 0;
}
