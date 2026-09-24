// A shared library for the multi-object translation test: a function and a
// piece of data that the main program reaches across the object boundary.
int lib_add(int a, int b) { return a + b; }
long lib_value = 0x1234500;
int lib_table[4] = { 10, 20, 30, 40 };
int lib_sum(void) {
	int s = 0;
	for (int i = 0; i < 4; i++) s += lib_table[i];
	return s;
}
