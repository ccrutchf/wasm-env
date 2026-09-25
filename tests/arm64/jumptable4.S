// A jump table whose dispatch sequence directly follows a `ret`, so it is
// entered only by the bounds check's conditional branch; the table address
// is set up before a loop whose cases all branch back to its head (as in
// numpy's dtypemeta_initialize_struct_from_spec). Exits with
// 1 + 20 + 100 + 20 = 141.
	.text
	.global _start
_start:
	adrp	x0, input
	add	x0, x0, :lo12:input
	mov	x19, #0
	adrp	x4, table		// the table, set up once
	add	x4, x4, :lo12:table
	bl	walk
	mov	x0, x19
	mov	x8, #94			// exit_group
	svc	#0
walk:
loop:
	ldr	w1, [x0], #4
	cbz	w1, done
	sub	w1, w1, #10
	cmp	w1, #3
	b.ls	dispatch
	b	loop
done:
	ret
dispatch:
	ldrb	w1, [x4, w1, uxtw]
	adr	x3, base
	add	x1, x3, w1, sxtb #2
	br	x1
case_before:
	add	x19, x19, #20
	b	loop
base:
	add	x19, x19, #1
	b	loop
case2:
	add	x19, x19, #100
	b	loop

	.section .rodata
input:
	.word	10, 11, 12, 13, 99, 0
table:
	.byte	(base - base) / 4, (case_before - base) / 4, (case2 - base) / 4, (case_before - base) / 4
