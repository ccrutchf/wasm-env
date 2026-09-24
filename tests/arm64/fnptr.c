// Discovery fixture: functions reached only through pointers.
//   - `table` holds pointers in data (found by scanning data words).
//   - `picked` is only ever loaded with ADRP+ADD (found in code).
// Neither is called directly, so a translator that only follows direct
// branches would miss them.
#include <stdio.h>

__attribute__((noinline)) int via_table_a(int x) { return x + 1; }
__attribute__((noinline)) int via_table_b(int x) { return x * 2; }
__attribute__((noinline)) int via_adrp(int x) { return x - 3; }

int (*const table[])(int) = { via_table_a, via_table_b };

int main(int argc, char **argv)
{
	(void)argv;
	int (*volatile picked)(int) = via_adrp;
	int r = table[argc & 1](argc) + picked(argc);
	printf("%d\n", r);
	return 0;
}
