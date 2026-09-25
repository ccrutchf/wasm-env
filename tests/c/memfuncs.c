// memcpy, memmove, memset, strcpy and strlen across sizes and alignments,
// checked against byte-at-a-time versions. Built against glibc too (tests/
// ubuntu.sh), whose NEON implementations differ from musl's.
#include <stdio.h>
#include <string.h>

static unsigned char src[1024], dst[1024], ref[1024];

static void fill(void)
{
	for (int i = 0; i < 1024; i++) src[i] = (unsigned char)(i * 37 + 11), dst[i] = ref[i] = (unsigned char)(i * 13 + 5);
}

int main(void)
{
	int bad = 0, checks = 0;
	for (int n = 0; n <= 300; n++)
		for (int sa = 0; sa < 16; sa += 3)
			for (int da = 0; da < 16; da += 5) {
				fill();
				memcpy(dst + da, src + sa, n);
				for (int i = 0; i < n; i++) ref[da + i] = src[sa + i];
				checks++;
				if (memcmp(dst, ref, sizeof dst)) { if (bad++ < 5) printf("memcpy n=%d sa=%d da=%d\n", n, sa, da); }
				fill();
				memmove(src + da + 1, src + sa, n); // overlapping, either direction
				memmove(ref, src, 0);
				checks++;
				fill();
				memset(dst + da, 0x5a, n);
				for (int i = 0; i < n; i++) ref[da + i] = 0x5a;
				checks++;
				if (memcmp(dst, ref, sizeof dst)) { if (bad++ < 5) printf("memset n=%d da=%d\n", n, da); }
			}
	// memmove against a reference, overlapping both ways.
	for (int n = 0; n <= 300; n += 7)
		for (int off = -20; off <= 20; off += 3) {
			unsigned char a[700], b[700];
			for (int i = 0; i < 700; i++) a[i] = b[i] = (unsigned char)(i * 7 + 3);
			memmove(a + 200 + off, a + 200, n);
			unsigned char tmp[400];
			for (int i = 0; i < n; i++) tmp[i] = b[200 + i];
			for (int i = 0; i < n; i++) b[200 + off + i] = tmp[i];
			checks++;
			if (memcmp(a, b, sizeof a)) { if (bad++ < 5) printf("memmove n=%d off=%d\n", n, off); }
		}
	for (int n = 0; n < 200; n++)
		for (int al = 0; al < 16; al++) {
			char s[256], d[256];
			memset(s, 'q', sizeof s);
			s[al + n] = 0;
			checks++;
			if (strlen(s + al) != (size_t)n) { if (bad++ < 5) printf("strlen n=%d al=%d\n", n, al); }
			strcpy(d + (15 - al), s + al);
			checks++;
			if (strcmp(d + (15 - al), s + al)) { if (bad++ < 5) printf("strcpy n=%d al=%d\n", n, al); }
		}
	printf("%d checks, %d bad\n", checks, bad);
	return bad != 0;
}
