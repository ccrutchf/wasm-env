// Runs glibc's NEON strrchr (tests/arm64/glibc_strrchr.S) over strings of
// many lengths and alignments, and checks it against a plain version.
#include <stdio.h>
#include <string.h>

char *glibc_strrchr(const char *, int);

static const char *plain(const char *s, int c)
{
	const char *r = 0;
	do if (*s == (char)c) r = s; while (*s++);
	return r;
}

int main(void)
{
	static char buf[256] __attribute__((aligned(64)));
	int bad = 0, checks = 0;
	for (int align = 0; align < 16; align++)
		for (int len = 0; len < 80; len++) {
			char *s = buf + align;
			memset(buf, 'z', sizeof buf);
			for (int i = 0; i < len; i++) s[i] = "abcx"[i % 4];
			s[len] = 0;
			for (const char *c = "axq"; *c; c++) {
				checks++;
				if (glibc_strrchr(s, *c) != plain(s, *c)) {
					if (bad++ < 5) printf("mismatch: align %d len %d char %c\n", align, len, *c);
				}
			}
			checks++;
			if (glibc_strrchr(s, 0) != s + len) bad++;
		}
	printf("%d checks, %d mismatches\n", checks, bad);
	return bad != 0;
}
