// GCC's computed goto with a table of label offsets ("labels as values"),
// as glibc's printf uses: a byte table maps the input to an index, and a
// table of 32-bit offsets from a base label gives the target.
#include <stdio.h>

static int __attribute__((noinline)) run(const char *s)
{
	static const unsigned char kind[128] = { ['a'] = 1, ['b'] = 2, ['c'] = 3, ['d'] = 4, ['e'] = 5, ['f'] = 6 };
	static const int jump[] = { &&other - &&other, &&add1 - &&other, &&add10 - &&other, &&double_it - &&other,
				    &&negate - &&other, &&square - &&other, &&reset - &&other };
	int acc = 0;
	for (; *s; s++) {
		goto *(&&other + jump[kind[*s & 127]]);
	add1:
		acc += 1;
		continue;
	add10:
		acc += 10;
		continue;
	double_it:
		acc *= 2;
		continue;
	negate:
		acc = -acc;
		continue;
	square:
		acc *= acc;
		continue;
	reset:
		acc = 0;
		continue;
	other:
		acc += 1000;
	}
	return acc;
}

int main(int argc, char **argv)
{
	printf("%d\n", run("abcabdz"));
	printf("%d\n", run(argc > 2 ? "eeaf" : "x"));
	printf("%d\n", run("fbbcde"));
	return 0;
}
