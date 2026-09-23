// Stack-based bytecode interpreter: dispatch-heavy code, the shape of
// bash, Python and Lua inner loops. The program is built at setup time so
// the compiler can't specialize run() for it.
#include "bench.h"

#define ITERATIONS 20000000

enum { PUSH, LOAD, STORE, ADD, MUL, XOR, LT, JZ, JMP, HALT };

struct insn {
  uint32_t op;
  int32_t arg;
};

static struct insn *code;
static int64_t *stack, *locals;

void setup(void) {
  static const struct insn prog[] = {
      {PUSH, 0}, {STORE, 0},           // i = 0
      {PUSH, 0}, {STORE, 1},           // s = 0
      {LOAD, 0}, {PUSH, ITERATIONS},   // 4: loop: i < N ?
      {LT, 0},   {JZ, 21},
      {LOAD, 1}, {LOAD, 0}, {PUSH, 3}, // s = (s + i*3) ^ i
      {MUL, 0},  {ADD, 0},  {LOAD, 0},
      {XOR, 0},  {STORE, 1},
      {LOAD, 0}, {PUSH, 1}, {ADD, 0},  // i = i + 1
      {STORE, 0}, {JMP, 4},
      {LOAD, 1}, {HALT, 0},            // 21: return s
  };
  code = bench_alloc(sizeof(prog));
  for (uint32_t i = 0; i < sizeof(prog) / sizeof(prog[0]); i++)
    code[i] = prog[i];
  stack = bench_alloc(sizeof(int64_t) * 64);
  locals = bench_alloc(sizeof(int64_t) * 8);
}

uint64_t run(void) {
  const struct insn *pc = code;
  int64_t *sp = stack;
  for (;;) {
    const struct insn in = *pc++;
    switch (in.op) {
    case PUSH: *sp++ = in.arg; break;
    case LOAD: *sp++ = locals[in.arg]; break;
    case STORE: locals[in.arg] = *--sp; break;
    case ADD: sp--; sp[-1] += sp[0]; break;
    case MUL: sp--; sp[-1] *= sp[0]; break;
    case XOR: sp--; sp[-1] ^= sp[0]; break;
    case LT: sp--; sp[-1] = sp[-1] < sp[0]; break;
    case JZ: if (!*--sp) pc = code + in.arg; break;
    case JMP: pc = code + in.arg; break;
    case HALT: return (uint64_t)sp[-1];
    default: __builtin_unreachable();
    }
  }
}
