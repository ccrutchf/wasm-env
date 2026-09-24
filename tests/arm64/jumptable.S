// Jump-table idioms block discovery must recognize (both seen in Alpine's
// BusyBox): entries interpreted through the add's sign extension, so a case
// can sit before the base label; and a table address the compiler spilled
// to the stack and reloads inside the loop. Exits with the sum of the cases
// taken (1 + 100 + 10 + 100 = 211); a missed case traps instead.
	.text
	.global _start
_start:
	mov	x19, #0			// sum
	mov	x20, #0			// i
	adrp	x0, table
	add	x0, x0, :lo12:table
	sub	sp, sp, #16
	str	x0, [sp, #8]		// spill the table address
loop:
	cmp	x20, #4
	b.hs	done
	ldr	x0, [sp, #8]		// reload it
	ldrh	w1, [x0, w20, uxtw #1]
	adr	x2, base
	add	x1, x2, w1, sxth #2
	br	x1
case_before:				// a negative table entry
	add	x19, x19, #100
	b	next
base:
case0:
	add	x19, x19, #1
	b	next
case1:
	add	x19, x19, #10
next:
	add	x20, x20, #1
	b	loop
done:
	mov	x0, x19
	mov	x8, #94			// exit_group
	svc	#0

	.section .rodata
	.balign 2
table:
	.hword	(case0 - base) / 4
	.hword	(case_before - base) / 4
	.hword	(case1 - base) / 4
	.hword	(case_before - base) / 4
