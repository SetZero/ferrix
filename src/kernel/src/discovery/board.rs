/// A peripheral a board's support found in the device tree and made ready
/// for a driver: what a [`BoardBinding`]'s `prepare` hands back.
#[derive(Debug)]
pub(crate) struct BoardDevice {
    /// Its register ranges, `(phys, len)`, in the order its driver maps
    /// them. Each must become an aperture, or the device is left alone.
    pub(crate) registers: Vec<(u64, u64)>,
    /// Its interrupt, as the tree gives it.
    pub(crate) interrupt: GicInterrupt,
    /// How it reaches memory.
    pub(crate) dma: DmaShape,
    /// What preparing it did, for the boot log.
    pub(crate) summary: String,
}

/// What preparing a board's peripheral for a driver is: find it in the
/// tree, clock it and take it out of reset, and say where it is. `Ok(None)`
/// on a machine that does not have it; `Err` says what stopped it.
pub(crate) type Prepare = fn(&Fdt<'_>) -> Result<Option<BoardDevice>, &'static str>;

/// The one clock of a board's peripheral a driver may ask for: the rate
/// wanted, and whether to set it or only ask what it would be. The rate it
/// is, or would be.
pub(crate) type Clock = fn(u64, bool) -> Result<u64, &'static str>;

/// A board's support for one device-tree binding the kernel knows, which
/// the board registers at bring-up with [`register_board`].
///
/// The core cannot name the board: board support is uncertified load, and a
/// registry that called into it would have put it in the core. So the board
/// hands the registry this instead, and the registry mints the node's
/// apertures and vector from what `prepare` says, under the same rules as
/// every other node's.
#[derive(Debug)]
pub(crate) struct BoardBinding {
    /// Which binding it is: `TREE_STM32_HDMI` and the rest.
    pub(crate) binding: u16,
    /// What the boot log's lines about it start with: `display`, `usb`.
    pub(crate) label: &'static str,
    /// What is left alone when it cannot be handed over, in a sentence:
    /// "the board's USB host".
    pub(crate) device: &'static str,
    /// Find it and make it ready.
    pub(crate) prepare: Prepare,
    /// Its clock a driver may set, if it has one: `device_clock`.
    pub(crate) clock: Option<Clock>,
}

/// Every registered [`BoardBinding`], in the order their nodes are published.
/// Eight is five more than the one board Ferrix knows registers.
static BOARD: Hooks<BoardBinding, 8> = Hooks::new();

/// Publish a node for `binding`'s peripheral whenever the tree has it,
/// after every binding registered before it.
///
/// Registered before [`publish`] runs, which `main.rs`'s bring-up order
/// makes so; a binding registered after it is never asked.
///
/// # Errors
///
/// [`Full`] when the list is.
pub(crate) fn register_board(binding: &'static BoardBinding) -> Result<(), Full> {
    BOARD.register(binding)
}

/// How many board bindings are registered, for the boot's check that board
/// support registered before enumeration.
pub(crate) fn board_bindings() -> usize {
    BOARD.len()
}

/// Ask for `node`'s clock at `hz`, setting it when `set` is: `None` when the
/// node's binding has no clock a driver may ask for.
pub(crate) fn board_clock(
    node: &DeviceNode,
    hz: u64,
    set: bool,
) -> Option<Result<u64, &'static str>> {
    let binding = node.tree_binding()?;
    let clock = BOARD
        .iter()
        .find(|registered| registered.binding == binding)?
        .clock?;
    Some(clock(hz, set))
}

/// A registered board binding's peripheral, when the tree has one: its
/// registers and interrupt, once the board's `prepare` has made it ready.
fn board_node(
    tree: &Fdt<'_>,
    board: &BoardBinding,
    reserved: &Reserved,
    held: &mut BTreeSet<u32>,
) -> Option<DeviceNode> {
    let label = board.label;
    let device = board.device;
    let prepared = match (board.prepare)(tree) {
        Ok(found) => found?,
        Err(why) => {
            crate::console::println!("  {label:<8} {device} is left alone: {why}");
            return None;
        }
    };
    let first = prepared.registers.first()?;
    let mut node = DeviceNode::empty(Location::Tree(first.0));
    node.binding = board.binding;
    node.dma = prepared.dma;
    for &(phys, len) in &prepared.registers {
        node.mint(phys, len, false, reserved);
    }
    if node.apertures.len() != prepared.registers.len() {
        crate::console::println!(
            "  {label:<8} {device} is left alone: its registers overlap memory the kernel uses"
        );
        return None;
    }
    let interrupt = prepared.interrupt;
    let usable = interrupt.id >= FIRST_SHARED_INTERRUPT
        && !irq::is_registered(interrupt.id)
        // FATAL-ALLOC: boot only: stage 10 builds the device registry once, before any program runs.
        && held.insert(interrupt.id);
    if !usable {
        crate::console::println!(
            "  {label:<8} {device} is left alone: interrupt {} is taken",
            interrupt.id
        );
        return None;
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
    crate::console::println!("  {label:<8} {}", prepared.summary);
    Some(node)
}
