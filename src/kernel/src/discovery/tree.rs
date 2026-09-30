/// Every `virtio,mmio` node in the device tree, on a machine without ACPI.
///
/// Its vectors are decoded only when the tree's interrupt controller takes
/// three-cell GIC specifiers, which is every tree Ferrix boots with; a node's
/// `interrupt-parent` is not followed, so a machine with a second interrupt
/// controller would need that first. A vector is taken only if it is a shared
/// peripheral interrupt that no kernel handler and no earlier node holds. The
/// first that fails ends the node's list, as a bad specifier does, because a
/// driver asks for its interrupts by position.
fn tree_nodes(view: &BootView<'_>, reserved: &Reserved) -> Vec<DeviceNode> {
    let mut nodes = Vec::new();
    let Description::Tree(tree) = description::of(view) else {
        return nodes;
    };
    let gic = tree
        .interrupt_controller()
        .is_some_and(|controller| controller.interrupt_cells() == Some(3));
    let mut held = BTreeSet::new();

    for found in tree.compatible_nodes(VIRTIO_MMIO_COMPATIBLE) {
        let Some(region) = found.reg().next() else {
            continue;
        };
        let mut node = DeviceNode::empty(Location::VirtioMmio(region.address));
        node.mint(region.address, region.size, false, reserved);
        if gic {
            let mut interrupts = found.gic_interrupts();
            for interrupt in interrupts.by_ref() {
                let usable = interrupt.id >= FIRST_SHARED_INTERRUPT
                    && !irq::is_registered(interrupt.id)
                    // FATAL-ALLOC: boot only: stage 10 builds the device registry once, before any program runs.
                    && held.insert(interrupt.id);
                if !usable {
                    node.withheld_vectors += 1;
                    break;
                }
                // FATAL-ALLOC: boot only: stage 10 builds the device registry once, before any program runs.
                node.vectors.push(Vector {
                    number: interrupt.id,
                    trigger: interrupt.trigger.map(|trigger| match trigger {
                        TreeTrigger::EdgeRising | TreeTrigger::EdgeFalling => Trigger::Edge,
                        TreeTrigger::LevelHigh | TreeTrigger::LevelLow => Trigger::Level,
                    }),
                    masking: Masking::Controller,
                });
            }
            node.withheld_vectors += interrupts.count();
        }
        // FATAL-ALLOC: boot only: stage 10 builds the device registry once, before any program runs.
        nodes.push(node);
    }
    for board in BOARD.iter() {
        if let Some(node) = board_node(&tree, board, reserved, &mut held) {
            // FATAL-ALLOC: boot only: stage 10 builds the device registry once, before any program runs.
            nodes.push(node);
        }
    }
    nodes
}
