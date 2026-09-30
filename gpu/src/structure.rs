//! A function's control flow as structured code, for WGSL, which has loops
//! and ifs but no gotos. This is Ramsey's translation ("Beyond Relooper",
//! 2022) of a reducible control-flow graph by its dominator tree: a block
//! that more than one edge enters (a merge) becomes the end of a labelled
//! `Block` its predecessors exit; a loop header becomes a `Loop` that back
//! edges restart; any other block is placed where its one predecessor
//! branches to it. Irreducible control flow is reported.
//!
//! WGSL has no labelled jumps, so wgsl.rs makes each `Block` and `Loop`
//! a WGSL loop and jumps past the innermost one through a variable. Jumps
//! at the end of their construct, the usual case, need neither: `tidy`
//! turns them into fallthrough, and a `Block` nothing jumps out of any more
//! is just its contents (an if/else's join).

use crate::ir::{Function, Operand, Res, Term, bail};

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    /// A block's instructions (not its terminator).
    Code(usize),
    Seq(Vec<Node>),
    /// Code that `Jump`s to `label` leave, continuing after it (with the
    /// block `label`, next in the enclosing sequence).
    Block(usize, Box<Node>),
    /// Code that `Jump`s to `label` (its header) restart.
    Loop(usize, Box<Node>),
    If(Operand, Box<Node>, Box<Node>),
    /// A switch's case: if the value is the constant, the first, else the
    /// second.
    Case(Operand, u64, Box<Node>, Box<Node>),
    /// The edge from..to taken: `to`'s phis get their values, then control
    /// jumps (to a `Block`'s end or a `Loop`'s start).
    Jump { from: usize, to: usize },
    /// The edge from..to taken, and `to`'s code follows directly.
    Edge { from: usize, to: usize },
    Return(Option<Operand>),
    Unreachable,
}

pub fn structure(f: &Function) -> Res<Node> {
    let n = f.blocks.len();
    let succs: Vec<Vec<usize>> = f.blocks.iter().map(|b| successors(&b.term)).collect();
    // Reverse postorder from the entry.
    let mut rpo = Vec::with_capacity(n);
    let mut seen = vec![false; n];
    let mut stack = vec![(0usize, 0usize)];
    seen[0] = true;
    while let Some(&mut (b, ref mut next)) = stack.last_mut() {
        if let Some(&s) = succs[b].get(*next) {
            *next += 1;
            if !seen[s] {
                seen[s] = true;
                stack.push((s, 0));
            }
        } else {
            rpo.push(b);
            stack.pop();
        }
    }
    rpo.reverse();
    let mut order = vec![usize::MAX; n];
    for (i, &b) in rpo.iter().enumerate() {
        order[b] = i;
    }
    let mut preds = vec![Vec::new(); n];
    for &b in &rpo {
        for &s in &succs[b] {
            preds[s].push(b);
        }
    }
    // Immediate dominators (Cooper, Harvey and Kennedy).
    let mut idom = vec![usize::MAX; n];
    idom[0] = 0;
    let mut changed = true;
    while changed {
        changed = false;
        for &b in &rpo[1..] {
            let mut new = usize::MAX;
            for &p in &preds[b] {
                if idom[p] == usize::MAX {
                    continue;
                }
                new = if new == usize::MAX { p } else { intersect(&idom, &order, p, new) };
            }
            if new != idom[b] {
                idom[b] = new;
                changed = true;
            }
        }
    }
    let dominates = |a: usize, mut b: usize| loop {
        if a == b {
            return true;
        }
        if b == 0 {
            return false;
        }
        b = idom[b];
    };
    let mut header = vec![false; n];
    let mut forward_in = vec![0usize; n];
    for &b in &rpo {
        for &s in &succs[b] {
            if order[s] <= order[b] {
                if !dominates(s, b) {
                    bail!("irreducible control flow");
                }
                header[s] = true;
            } else {
                forward_in[s] += 1;
            }
        }
    }
    let merge: Vec<bool> = forward_in.iter().map(|&k| k >= 2).collect();
    let mut children = vec![Vec::new(); n];
    for &b in &rpo[1..] {
        children[idom[b]].push(b);
    }
    let s = Structurer { f, order, header, merge, children };
    let tree = s.tree(0);
    Ok(tidy(tree))
}

fn intersect(idom: &[usize], order: &[usize], mut a: usize, mut b: usize) -> usize {
    while a != b {
        while order[a] > order[b] {
            a = idom[a];
        }
        while order[b] > order[a] {
            b = idom[b];
        }
    }
    a
}

pub fn successors(t: &Term) -> Vec<usize> {
    match t {
        Term::Br(b) => vec![*b],
        Term::CondBr(_, a, b) => vec![*a, *b],
        Term::Switch(_, d, cases) => std::iter::once(*d).chain(cases.iter().map(|c| c.1)).collect(),
        Term::Ret(_) | Term::Unreachable => Vec::new(),
    }
}

struct Structurer<'a> {
    f: &'a Function,
    order: Vec<usize>,
    header: Vec<bool>,
    merge: Vec<bool>,
    children: Vec<Vec<usize>>,
}

impl Structurer<'_> {
    fn tree(&self, x: usize) -> Node {
        // The merges x dominates, latest first: the outermost Block ends
        // at the last of them.
        let mut ys: Vec<usize> = self.children[x].iter().copied().filter(|&c| self.merge[c]).collect();
        ys.sort_by_key(|&y| std::cmp::Reverse(self.order[y]));
        let code = self.within(x, &ys);
        if self.header[x] { Node::Loop(x, Box::new(code)) } else { code }
    }

    fn within(&self, x: usize, ys: &[usize]) -> Node {
        match ys.split_first() {
            None => {
                let branch = match &self.f.blocks[x].term {
                    Term::Br(t) => self.branch(x, *t),
                    Term::CondBr(c, t, e) => Node::If(c.clone(), Box::new(self.branch(x, *t)), Box::new(self.branch(x, *e))),
                    Term::Switch(v, d, cases) => {
                        // An if/else chain: WGSL's `break` in a switch
                        // would leave the switch, not a loop.
                        let mut node = self.branch(x, *d);
                        for (c, t) in cases.iter().rev() {
                            node = Node::Case(v.clone(), *c, Box::new(self.branch(x, *t)), Box::new(node));
                        }
                        node
                    }
                    Term::Ret(v) => Node::Return(v.clone()),
                    Term::Unreachable => Node::Unreachable,
                };
                Node::Seq(vec![Node::Code(x), branch])
            }
            Some((&y, rest)) => Node::Seq(vec![Node::Block(y, Box::new(self.within(x, rest))), self.tree(y)]),
        }
    }

    fn branch(&self, from: usize, to: usize) -> Node {
        if self.order[to] <= self.order[from] || self.merge[to] {
            Node::Jump { from, to }
        } else {
            Node::Seq(vec![Node::Edge { from, to }, self.tree(to)])
        }
    }
}

/// Jumps at the end of the construct they target become fallthrough, and
/// Blocks nothing jumps out of any more become their contents.
pub fn tidy(node: Node) -> Node {
    match node {
        Node::Seq(items) => {
            let mut out = Vec::new();
            for i in items {
                match tidy(i) {
                    Node::Seq(inner) => out.extend(inner),
                    i => out.push(i),
                }
            }
            if out.len() == 1 { out.pop().unwrap() } else { Node::Seq(out) }
        }
        Node::Block(label, body) => {
            let body = tail(tidy(*body), label);
            if jumps_to(&body, label) { Node::Block(label, Box::new(body)) } else { body }
        }
        Node::Loop(label, body) => Node::Loop(label, Box::new(tail(tidy(*body), label))),
        Node::If(c, a, b) => Node::If(c, Box::new(tidy(*a)), Box::new(tidy(*b))),
        Node::Case(v, k, a, b) => Node::Case(v, k, Box::new(tidy(*a)), Box::new(tidy(*b))),
        n => n,
    }
}

/// `node` with jumps to `label` in tail position made fallthrough.
fn tail(node: Node, label: usize) -> Node {
    match node {
        Node::Jump { from, to } if to == label => Node::Edge { from, to },
        Node::Seq(mut items) => {
            if let Some(last) = items.pop() {
                items.push(tail(last, label));
            }
            Node::Seq(items)
        }
        Node::If(c, a, b) => Node::If(c, Box::new(tail(*a, label)), Box::new(tail(*b, label))),
        Node::Case(v, k, a, b) => Node::Case(v, k, Box::new(tail(*a, label)), Box::new(tail(*b, label))),
        n => n,
    }
}

fn jumps_to(node: &Node, label: usize) -> bool {
    match node {
        Node::Jump { to, .. } => *to == label,
        Node::Seq(items) => items.iter().any(|i| jumps_to(i, label)),
        Node::Block(_, b) | Node::Loop(_, b) => jumps_to(b, label),
        Node::If(_, a, b) | Node::Case(_, _, a, b) => jumps_to(a, label) || jumps_to(b, label),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::parse;

    fn shape(n: &Node) -> String {
        match n {
            Node::Code(b) => format!("{b}"),
            Node::Seq(items) => items.iter().map(shape).collect::<Vec<_>>().join(" "),
            Node::Block(l, b) => format!("block{l}[{}]", shape(b)),
            Node::Loop(l, b) => format!("loop{l}[{}]", shape(b)),
            Node::If(_, a, b) | Node::Case(_, _, a, b) => format!("if({} | {})", shape(a), shape(b)),
            Node::Jump { to, .. } => format!("->{to}"),
            Node::Edge { .. } => String::new(),
            Node::Return(_) => "ret".into(),
            Node::Unreachable => "unreachable".into(),
        }
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
    }

    fn structured(body: &str) -> String {
        let m = parse(&format!("define spir_kernel void @k(i32 %0, i1 %1) {{\n{body}\n}}\n")).unwrap();
        shape(&structure(&m.functions[0]).unwrap())
    }

    #[test]
    fn an_if_else_joins_without_a_block() {
        let s = structured(
            "  br i1 %1, label %3, label %4\n3:\n  br label %5\n4:\n  br label %5\n5:\n  ret void",
        );
        assert_eq!(s, "0 if(1 | 2) 3 ret");
    }

    #[test]
    fn a_loop_restarts_and_exits() {
        // 0 -> 1 (header) -> 2 (body) -> 1, 1 -> 3 (exit).
        let s = structured("  br label %3\n3:\n  br i1 %1, label %4, label %5\n4:\n  br label %3\n5:\n  ret void");
        assert_eq!(s, "0 loop1[1 if(2 | 3 ret)]");
    }

    #[test]
    fn a_jump_out_of_a_loop_to_a_shared_block() {
        // The loop (1, 2) leaves for 4 from both its blocks; 0 reaches 4 too.
        let s = structured(
            "  br i1 %1, label %3, label %6\n3:\n  br i1 %1, label %4, label %6\n4:\n  br i1 %1, label %3, label %6\n6:\n  ret void",
        );
        assert_eq!(s, "block3[0 if(loop1[1 if(2 if( | ->3) | ->3)] | )] 3 ret");
    }

    #[test]
    fn irreducible_flow_is_reported() {
        let m = parse("define spir_kernel void @k(i1 %0) {\n  br i1 %0, label %2, label %3\n2:\n  br label %3\n3:\n  br label %2\n}\n").unwrap();
        assert_eq!(structure(&m.functions[0]), Err("irreducible control flow".into()));
    }
}
