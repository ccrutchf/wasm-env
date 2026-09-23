// Binary search tree lookups. Node size is 12 bytes on wasm32 and 24 on
// wasm64/native, so this shows the cache cost of 64-bit pointers.
#include "bench.h"

#define N (1u << 20)
#define LOOKUPS (1u << 20)

struct node {
  struct node *left, *right;
  uint32_t key;
};

static struct node *root;

void setup(void) {
  struct node *nodes = bench_alloc(sizeof(struct node) * N);
  root = 0;
  uint64_t s = 2;
  for (uint32_t i = 0; i < N; i++) {
    struct node *n = &nodes[i];
    n->key = (uint32_t)rng_next(&s);
    struct node **slot = &root;
    while (*slot)
      slot = n->key < (*slot)->key ? &(*slot)->left : &(*slot)->right;
    *slot = n;
  }
}

uint64_t run(void) {
  uint64_t s = 2, found = 0, depth = 0;
  for (uint32_t i = 0; i < LOOKUPS; i++) {
    // Half the lookups replay inserted keys (hits); half are random.
    uint32_t key = (i & 1) ? (uint32_t)rng_next(&s) : (uint32_t)(i * 2654435761u);
    struct node *n = root;
    while (n && n->key != key) {
      n = key < n->key ? n->left : n->right;
      depth++;
    }
    found += n != 0;
  }
  return found * 1000003 + depth;
}
