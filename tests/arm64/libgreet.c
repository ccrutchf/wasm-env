// A shared library that uses libc, with a constructor, thread-local state
// and exported data: loaded at run time by the (translated) dynamic linker.
#include <stdio.h>
#include <string.h>

int greet_ready;
static __thread int calls;

__attribute__((constructor)) static void setup(void) { greet_ready = 42; }

const char *greet(const char *who)
{
	static char buf[64];
	calls++;
	snprintf(buf, sizeof buf, "hello, %s (call %d)", who, calls);
	return buf;
}

size_t greet_len(const char *s) { return strlen(s); }
