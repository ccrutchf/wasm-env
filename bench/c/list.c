// Linked-list walk over nodes linked in random order.
// Stresses pointer size (8 vs 16 bytes per node) and cache misses.
#include "bench.h"

#define N (1u << 21)
#define PASSES 4

struct node {
  struct node *next;
  uint32_t val;
};

static struct node *head;

void setup(void) {
  struct node *nodes = bench_alloc(sizeof(struct node) * N);
  uint32_t *perm = bench_alloc(sizeof(uint32_t) * N);
  uint64_t s = 1;
  for (uint32_t i = 0; i < N; i++)
    perm[i] = i;
  for (uint32_t i = N - 1; i > 0; i--) {
    uint32_t j = (uint32_t)(rng_next(&s) % (i + 1));
    uint32_t t = perm[i];
    perm[i] = perm[j];
    perm[j] = t;
  }
  for (uint32_t i = 0; i < N; i++) {
    nodes[perm[i]].val = i;
    nodes[perm[i]].next = i + 1 < N ? &nodes[perm[i + 1]] : 0;
  }
  head = &nodes[perm[0]];
}

uint64_t run(void) {
  uint64_t sum = 0;
  for (int p = 0; p < PASSES; p++)
    for (struct node *n = head; n; n = n->next)
      sum += n->val ^ (uint32_t)p;
  return sum;
}
