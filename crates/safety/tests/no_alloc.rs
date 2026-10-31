//! The guard and preflight rules allocate nothing (#29): they run on every
//! item of a plan, right before it moves. A counting global allocator watches
//! this thread while the checks run.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use storagelens_core::{EntryKind, FileStamp};
use storagelens_safety::{compare, Guard, Platform, Refusal, Skip};

struct Counting;

thread_local! {
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

// SAFETY: forwards every call unchanged to the system allocator; the counter
// is a thread-local `Cell`, which itself never allocates.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.with(|count| count.set(count.get() + 1));
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn allocations(run: impl FnOnce()) -> usize {
    let before = ALLOCATIONS.with(Cell::get);
    run();
    ALLOCATIONS.with(Cell::get) - before
}

#[test]
fn the_rules_never_allocate() {
    let mac = Guard::from_resolved(Platform::MacOs, "/Users/me", Some("/Users/me")).expect("mac");
    let win = Guard::from_resolved(Platform::Windows, r"C:\", Some(r"C:\Users\me")).expect("win");
    let scanned = FileStamp {
        kind: EntryKind::File,
        size_logical: 10,
        modified: 1,
        dev: 1,
        file_id: 1,
        attrs: 0,
    };

    let count = allocations(|| {
        for _ in 0..1_000 {
            assert_eq!(
                mac.check_resolved("/Users/me/Library/Caches/com.x/blob"),
                Ok(())
            );
            assert_eq!(
                mac.check_resolved("/Users/me/Library/Keychains/login.keychain-db"),
                Err(Refusal::Protected)
            );
            assert_eq!(
                mac.check_resolved("/Users/me/Old.app/Contents/x"),
                Err(Refusal::BundleInternal)
            );
            assert_eq!(mac.check_resolved("/usr/bin/x"), Err(Refusal::OutsideRoot));
            assert_eq!(
                win.check_resolved(r"\\?\C:\Program Files\x"),
                Err(Refusal::System(r"C:\Program Files"))
            );
            assert_eq!(win.check_resolved(r"C:\Games\cache.bin"), Ok(()));
            assert_eq!(compare(Platform::MacOs, &scanned, Ok(scanned)), Ok(()));
            assert_eq!(
                compare(
                    Platform::MacOs,
                    &scanned,
                    Ok(FileStamp {
                        file_id: 2,
                        ..scanned
                    })
                ),
                Err(Skip::Replaced)
            );
        }
    });
    assert_eq!(count, 0, "the fast path allocated {count} times");

    // The counter works: building a guard allocates.
    assert!(allocations(|| drop(Guard::from_resolved(Platform::MacOs, "/", None))) > 0);
}
