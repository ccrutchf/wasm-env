// Differential fixture: loads and stores of every width and signedness,
// addressing modes (pre/post-index, register offsets with extension),
// pairs, struct copies, unaligned access, and libc's string functions
// (musl's arm64 memcpy/memset use 128-bit SIMD loads and stores).
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

struct rec { uint8_t b; int16_t h; int32_t w; int64_t x; char name[13]; };

static volatile int8_t sbytes[] = { -128, -1, 0, 1, 127 };
static volatile int16_t shalves[] = { -32768, -1, 0, 1, 32767 };
static volatile int32_t swords[] = { INT32_MIN, -1, 0, 1, INT32_MAX };

__attribute__((noinline)) static uint64_t checksum(const uint8_t *p, size_t n)
{
	uint64_t h = 1469598103934665603ull;
	while (n--) h = (h ^ *p++) * 1099511628211ull; // post-increment loads
	return h;
}

__attribute__((noinline)) static void fill(uint32_t *p, int n, uint32_t v)
{
	for (int i = 0; i < n; i++) *p++ = v + (uint32_t)i;
}

__attribute__((noinline)) static int64_t index_sum(const int32_t *base, const int32_t *idx, int n)
{
	int64_t s = 0;
	for (int i = 0; i < n; i++) s += base[idx[i]]; // register offset, sxtw, lsl #2
	return s;
}

int main(void)
{
	for (int i = 0; i < 5; i++)
		printf("sext %d %d %d -> %lld %lld %lld\n", i, sbytes[i], shalves[i], (long long)sbytes[i],
		       (long long)shalves[i], (long long)swords[i]);

	struct rec a = { 0xab, -1234, -123456789, 0x1122334455667788, "hello world!" };
	struct rec b;
	memcpy(&b, &a, sizeof a);
	b.h++;
	b.name[0] = 'J';
	printf("rec %u %d %d %llx %s\n", b.b, b.h, b.w, (unsigned long long)b.x, b.name);

	uint8_t *buf = malloc(4096 + 64);
	for (int i = 0; i < 4096 + 64; i++) buf[i] = (uint8_t)(i * 31 + 7);
	for (int off = 0; off < 17; off++) {
		uint64_t u;
		memcpy(&u, buf + off, 8); // unaligned
		printf("unaligned %d %016llx\n", off, (unsigned long long)u);
	}
	memmove(buf + 3, buf, 1000);
	memset(buf + 2000, 0x5a, 777);
	printf("checksum %016llx\n", (unsigned long long)checksum(buf, 4096 + 64));

	uint32_t words[100];
	fill(words, 100, 0xfffffff0);
	int32_t idx[] = { 5, 99, 0, 42, 42, 17 };
	printf("index_sum %lld\n", (long long)index_sum((const int32_t *)words, idx, 6));

	char s[64];
	strcpy(s, "The quick brown fox");
	strcat(s, " jumps");
	printf("strlen %zu strcmp %d strchr %s\n", strlen(s), strcmp(s, "The quick"), strchr(s, 'b'));
	printf("snprintf [%10.3s|%-5d|%x|%c]\n", "abcdef", 42, 0xbeefu, 'z');
	free(buf);
	return 0;
}
