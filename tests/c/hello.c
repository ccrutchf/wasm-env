#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <setjmp.h>

static jmp_buf jb;

static void jump(int v) { longjmp(jb, v); }

__attribute__((constructor)) static void ctor(void) { puts("constructor ran"); }

int main(int argc, char **argv)
{
	printf("hello from wasm64-linux-musl, argc=%d\n", argc);
	for (int i = 0; i < argc; i++)
		printf("  argv[%d] = %s\n", i, argv[i]);
	const char *home = getenv("HOME");
	printf("HOME=%s\n", home ? home : "(unset)");

	char *p = malloc(1 << 20);
	strcpy(p, "malloc works");
	puts(p);
	free(p);

	printf("long double: %Lf, 128-bit: %llu\n", (long double)1 / 3,
	       (unsigned long long)((unsigned __int128)1 << 100 >> 90));

	int r = setjmp(jb);
	if (r == 0)
		jump(42);
	printf("longjmp returned %d\n", r);
	return 7;
}
