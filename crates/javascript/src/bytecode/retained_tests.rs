use super::*;

fn retained(chunk: Rc<Chunk>) -> usize {
    retained_size_with_chunks(vec![], vec![chunk])
}

#[test]
fn retained_bytecode_counts_spare_capacity_and_shared_roots() {
    let empty = retained(Rc::default());
    let mut chunk = Chunk {
        ops: Vec::with_capacity(257),
        ..Default::default()
    };
    chunk.ops.push(Op::Pop);
    let expected = empty + chunk.ops.capacity() * std::mem::size_of::<Op>();
    let root = Rc::new(chunk);
    assert_eq!(retained(root.clone()), expected);
    assert_eq!(
        retained_size_with_chunks(vec![], vec![root.clone(), root]),
        expected
    );
}

#[test]
fn retained_bytecode_counts_nested_opcode_and_scope_storage() {
    let child = Rc::<Chunk>::default();
    let size = |extra| {
        let mut cases = Vec::with_capacity(extra + 1);
        cases.push((None, child.clone()));
        let mut lexical = Vec::with_capacity(extra + 1);
        lexical.push(("loop".into(), false));
        let mut scope = ScopePlan {
            vars: Vec::with_capacity(extra + 1),
            ..Default::default()
        };
        scope.vars.push("case".into());
        let ops = vec![
            Op::For {
                initial: None,
                test: None,
                update: None,
                body: child.clone(),
                lexical,
                label: None,
            },
            Op::Switch(child.clone(), cases, Box::new(scope)),
            Op::Branch(vec![(child.clone(), child.clone()); extra + 1].into(), None),
        ];
        retained(Rc::new(Chunk {
            ops,
            ..Default::default()
        }))
    };
    let extra = 137;
    let bytes_per_entry = std::mem::size_of::<(Option<Rc<Chunk>>, Rc<Chunk>)>()
        + std::mem::size_of::<(JsString, bool)>()
        + std::mem::size_of::<JsString>()
        + std::mem::size_of::<(Rc<Chunk>, Rc<Chunk>)>();
    assert_eq!(size(extra) - size(0), extra * bytes_per_entry);
}

#[test]
fn retained_destructuring_accounts_for_empty_array_slots_and_reserved_keys() {
    let size = |extra| {
        let items = vec![None; extra];
        let object = Vec::with_capacity(extra);
        let ops = vec![
            Op::Destructure(Rc::new(PatternCode::Array(items, None))),
            Op::Destructure(Rc::new(PatternCode::Object(object, None))),
        ];
        retained(Rc::new(Chunk {
            ops,
            ..Default::default()
        }))
    };
    let bytes_per_slot = std::mem::size_of::<Option<Rc<PatternCode>>>()
        + std::mem::size_of::<(Rc<Chunk>, Rc<PatternCode>)>();
    assert_eq!(size(259) - size(0), 259 * bytes_per_slot);
}
