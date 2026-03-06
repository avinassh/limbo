thread 'shuttle_test_lost_updates' panicked at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/shuttle-0.8.1/src/sync/atomic/mod.rs:215:20:
  already borrowed: BorrowMutError
  stack backtrace:
     0:        0x105f32a00 - std::backtrace_rs::backtrace::libunwind::trace::h397b10519d455313
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/std/src/../../backtrace/src/backtrace/libunwind.rs:117:9
     1:        0x105f32a00 - std::backtrace_rs::backtrace::trace_unsynchronized::h902ac8b376515985
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/std/src/../../backtrace/src/backtrace/mod.rs:66:14
     2:        0x105f32a00 - std::sys::backtrace::_print_fmt::hd7805902c8299639
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/std/src/sys/backtrace.rs:66:9
     3:        0x105f32a00 - <std::sys::backtrace::BacktraceLock::print::DisplayBacktrace as core::fmt::Display>::fmt::h2c3f7b4c4ce00f07
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/std/src/sys/backtrace.rs:39:26
     4:        0x105f50ee0 - core::fmt::rt::Argument::fmt::h1e6a80a47071126e
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/core/src/fmt/rt.rs:181:76
     5:        0x105f50ee0 - core::fmt::write::h1dbafa36e52e01c5
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/core/src/fmt/mod.rs:1446:25
     6:        0x105f2fd4c - std::io::default_write_fmt::h6bd5847749ac7605
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/std/src/io/mod.rs:639:11
     7:        0x105f2fd4c - std::io::Write::write_fmt::hee580fb33ba5bfa5
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/std/src/io/mod.rs:1914:13
     8:        0x105f328b4 - std::sys::backtrace::BacktraceLock::print::h1fb87370474572ed
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/std/src/sys/backtrace.rs:42:9
     9:        0x105f34174 - std::panicking::default_hook::{{closure}}::h195a9b2c829547eb
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/std/src/panicking.rs:300:22
    10:        0x105f33f88 - std::panicking::default_hook::h18c3aa3e3a3584d5
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/std/src/panicking.rs:324:9
    11:        0x104ed4cb4 - <alloc::boxed::Box<F,A> as core::ops::function::Fn<Args>>::call::h7bed841a58f409e3
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/alloc/src/boxed.rs:1980:9
    12:        0x104ed4cb4 - test::test_main_with_exit_callback::{{closure}}::h29a05f78049a974e
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/test/src/lib.rs:145:21
    13:        0x105ef760c - <alloc::boxed::Box<F,A> as core::ops::function::Fn<Args>>::call::ha4648d6786b43557
                                 at /Users/avi/.rustup/toolchains/1.88.0-aarch64-apple-darwin/lib/rustlib/src/rust/library/alloc/src/boxed.rs:1980:9
    14:        0x105ebdc18 - shuttle::runtime::failure::init_panic_hook::{{closure}}::{{closure}}::hf30f57e0f21b98b2
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/shuttle-0.8.1/src/runtime/failure.rs:151:13
    15:        0x105ef760c - <alloc::boxed::Box<F,A> as core::ops::function::Fn<Args>>::call::ha4648d6786b43557
                                 at /Users/avi/.rustup/toolchains/1.88.0-aarch64-apple-darwin/lib/rustlib/src/rust/library/alloc/src/boxed.rs:1980:9
    16:        0x105ef2460 - generator::detail::gen::catch_unwind_filter::{{closure}}::{{closure}}::haa336ae7b7c732d8
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/generator-0.8.8/src/detail/gen.rs:19:13
    17:        0x105f34c90 - <alloc::boxed::Box<F,A> as core::ops::function::Fn<Args>>::call::h9e486227024a0357
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/alloc/src/boxed.rs:1980:9
    18:        0x105f34c90 - std::panicking::rust_panic_with_hook::h02a9fa3cad928562
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/std/src/panicking.rs:841:13
    19:        0x105f34880 - std::panicking::begin_panic_handler::{{closure}}::hd1cc56578f819958
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/std/src/panicking.rs:706:13
    20:        0x105f32ec4 - std::sys::backtrace::__rust_end_short_backtrace::h52c1e479035e4bc4
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/std/src/sys/backtrace.rs:168:18
    21:        0x105f34528 - __rustc[4794b31dd7191200]::rust_begin_unwind
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/std/src/panicking.rs:697:5
    22:        0x105f995f4 - core::panicking::panic_fmt::heec96bfc27e6c546
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/core/src/panicking.rs:75:14
    23:        0x105f9952c - core::cell::panic_already_borrowed::ha05dfdc27881e579
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/core/src/cell.rs:791:5
    24:        0x105ee1c64 - core::cell::RefCell<T>::borrow_mut::h20ec4d7353017a8b
                                 at /Users/avi/.rustup/toolchains/1.88.0-aarch64-apple-darwin/lib/rustlib/src/rust/library/core/src/cell.rs:1083:25
    25:        0x105ea40c0 - shuttle::sync::atomic::Atomic<T>::init_clock::h5bbe38090d3a5c65
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/shuttle-0.8.1/src/sync/atomic/mod.rs:215:9
    26:        0x105ea464c - shuttle::sync::atomic::Atomic<T>::inhale_clock::h8c0ec67f26b2e55e
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/shuttle-0.8.1/src/sync/atomic/mod.rs:221:9
    27:        0x105ea2708 - shuttle::sync::atomic::Atomic<T>::fetch_update::h186a7db36f83f333
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/shuttle-0.8.1/src/sync/atomic/mod.rs:201:13
    28:        0x105ebeaa4 - shuttle::sync::atomic::int::AtomicU32::fetch_update::h4bedc13421d8c82d
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/shuttle-0.8.1/src/sync/atomic/int.rs:74:17
    29:        0x105ebead8 - shuttle::sync::atomic::int::AtomicU32::fetch_add::h814e6041cf47523e
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/shuttle-0.8.1/src/sync/atomic/int.rs:128:17
    30:        0x1059bb240 - turso_core::storage::buffer_pool::Arena::new::{{closure}}::hd5fad8f77be93093
                                 at /Users/avi/turso/limbo/core/storage/buffer_pool.rs:395:31
    31:        0x105933958 - core::result::Result<T,E>::unwrap_or_else::he1e98efe10c442e4
                                 at /Users/avi/.rustup/toolchains/1.88.0-aarch64-apple-darwin/lib/rustlib/src/rust/library/core/src/result.rs:1491:23
    32:        0x105403f50 - turso_core::storage::buffer_pool::Arena::new::he33fa9840c94151c
                                 at /Users/avi/turso/limbo/core/storage/buffer_pool.rs:391:18
    33:        0x1054028d8 - turso_core::storage::buffer_pool::PoolInner::init_arenas::he22f5bfb9e58ba46
                                 at /Users/avi/turso/limbo/core/storage/buffer_pool.rs:295:15
    34:        0x1054022b0 - turso_core::storage::buffer_pool::BufferPool::finalize_with_page_size::h4392a7fa931adb56
                                 at /Users/avi/turso/limbo/core/storage/buffer_pool.rs:233:13
    35:        0x1052bc6dc - turso_core::storage::pager::Pager::allocate_page1::h28c36bcc0deadfc0
                                 at /Users/avi/turso/limbo/core/storage/pager.rs:4349:17
    36:        0x1052a93d8 - turso_core::storage::pager::Pager::maybe_allocate_page1::h725d81eaaf059bad
                                 at /Users/avi/turso/limbo/core/storage/pager.rs:2538:27
    37:        0x1056135c8 - turso_core::storage::pager::Pager::begin_write_tx::hb9550465ef73c663
                                 at /Users/avi/turso/limbo/core/storage/pager.rs:2551:23
    38:        0x1056135c8 - turso_core::vdbe::execute::op_transaction_inner::hf5324e88dc7a8a1a
                                 at /Users/avi/turso/limbo/core/vdbe/execute.rs:2650:46
    39:        0x10560f8b8 - turso_core::vdbe::execute::op_transaction::h422c04ba84afefc9
                                 at /Users/avi/turso/limbo/core/vdbe/execute.rs:2354:18
    40:        0x10597691c - turso_core::vdbe::Program::normal_step::h0ac42d6172ce5344
                                 at /Users/avi/turso/limbo/core/vdbe/mod.rs:1221:19
    41:        0x10596d77c - turso_core::vdbe::Program::step::hb3bb33d223f1a694
                                 at /Users/avi/turso/limbo/core/vdbe/mod.rs:1000:34
    42:        0x1053eb080 - turso_core::statement::Statement::_step::hf8b095867c4261e9
                                 at /Users/avi/turso/limbo/core/statement.rs:164:13
    43:        0x1053ebbcc - turso_core::statement::Statement::step_with_waker::hb3057a705b58fc8a
                                 at /Users/avi/turso/limbo/core/statement.rs:236:9
    44:        0x104efae84 - turso_sdk_kit::rsapi::TursoStatement::step_no_guard::h6627d6327522d945
                                 at /Users/avi/turso/limbo/sdk-kit/src/rsapi.rs:1009:17
    45:        0x104efadc0 - turso_sdk_kit::rsapi::TursoStatement::step::hb0a485adbb93731a
                                 at /Users/avi/turso/limbo/sdk-kit/src/rsapi.rs:1002:9
    46:        0x104eded54 - turso::Statement::step::hb7eed40262822426
                                 at /Users/avi/turso/limbo/bindings/rust/src/lib.rs:321:15
    47:        0x104ede92c - <turso::Execute as core::future::future::Future>::poll::h1199d14527188d21
                                 at /Users/avi/turso/limbo/bindings/rust/src/lib.rs:304:15
    48:        0x104e86cf8 - turso::Statement::execute::{{closure}}::he5c421fd97b01ca1
                                 at /Users/avi/turso/limbo/bindings/rust/src/lib.rs:395:17
    49:        0x104ea2138 - turso::connection::Connection::execute::{{closure}}::hc31ee9cadb13bd62
                                 at /Users/avi/turso/limbo/bindings/rust/src/connection.rs:118:30
    50:        0x104e9647c - lost_updates::setup_mvcc_db::{{closure}}::h8294357029803f64
                                 at /Users/avi/turso/limbo/testing/stress/tests/lost_updates.rs:29:49
    51:        0x104e9735c - lost_updates::lost_updates_scenario::{{closure}}::hdfebf0802314869a
                                 at /Users/avi/turso/limbo/testing/stress/tests/lost_updates.rs:53:6
    52:        0x104e90ee4 - shuttle::future::block_on::h2db26edd5d7e93f9
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/shuttle-0.8.1/src/future/mod.rs:243:15
    53:        0x104e98704 - lost_updates::shuttle_test_lost_updates::{{closure}}::h882e85d3357d0546
                                 at /Users/avi/turso/limbo/testing/stress/tests/lost_updates.rs:107:19
    54:        0x104e8a138 - shuttle::runtime::runner::Runner<S>::run::{{closure}}::{{closure}}::{{closure}}::h1b96c3d78eb57c93
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/shuttle-0.8.1/src/runtime/runner.rs:106:101
    55:        0x104ea51b4 - shuttle::thread::thread_fn::hafab832c82fe35e6
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/shuttle-0.8.1/src/thread.rs:201:15
    56:        0x104e8ed3c - shuttle::runtime::execution::Execution::run::{{closure}}::{{closure}}::h50fd4b765cc29bfb
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/shuttle-0.8.1/src/runtime/execution.rs:83:34
    57:        0x104e9afbc - core::ops::function::FnOnce::call_once{{vtable.shim}}::hdb09b09bb7be02e8
                                 at /Users/avi/.rustup/toolchains/1.88.0-aarch64-apple-darwin/lib/rustlib/src/rust/library/core/src/ops/function.rs:250:5
    58:        0x105eef6d4 - <alloc::boxed::Box<F,A> as core::ops::function::FnOnce<Args>>::call_once::h1163b76569e97a04
                                 at /Users/avi/.rustup/toolchains/1.88.0-aarch64-apple-darwin/lib/rustlib/src/rust/library/alloc/src/boxed.rs:1966:9
    59:        0x105ec6544 - shuttle::runtime::thread::continuation::Continuation::new::{{closure}}::hc043a252e52f3f9f
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/shuttle-0.8.1/src/runtime/thread/continuation.rs:87:21
    60:        0x105ed10f8 - generator::gen_impl::GeneratorImpl<A,T>::init_code::{{closure}}::h1f64ec2622e53e8c
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/generator-0.8.8/src/gen_impl.rs:357:21
    61:        0x105ec56b0 - generator::stack::StackBox<F>::call_once::hfc88481d9a704768
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/generator-0.8.8/src/stack/mod.rs:137:13
    62:        0x105ef3210 - generator::stack::Func::call_once::h39573e5627669fa8
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/generator-0.8.8/src/stack/mod.rs:119:9
    63:        0x105ef25e4 - generator::detail::gen::gen_init_impl::{{closure}}::hac9963fc0dd568b1
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/generator-0.8.8/src/detail/gen.rs:35:9
    64:        0x105ef5160 - core::ops::function::FnOnce::call_once::hfbe2ad52aced7712
                                 at /Users/avi/.rustup/toolchains/1.88.0-aarch64-apple-darwin/lib/rustlib/src/rust/library/core/src/ops/function.rs:250:5
    65:        0x105ef4ad0 - std::panicking::try::do_call::h1dc989a35d7b4911
                                 at /Users/avi/.rustup/toolchains/1.88.0-aarch64-apple-darwin/lib/rustlib/src/rust/library/std/src/panicking.rs:589:40
    66:        0x105ef7bd4 - ___rust_try
    67:        0x105ef4a38 - std::panicking::try::hbaa2cb079860786a
                                 at /Users/avi/.rustup/toolchains/1.88.0-aarch64-apple-darwin/lib/rustlib/src/rust/library/std/src/panicking.rs:552:19
    68:        0x105ef4a38 - std::panic::catch_unwind::h983cb91582a67f64
                                 at /Users/avi/.rustup/toolchains/1.88.0-aarch64-apple-darwin/lib/rustlib/src/rust/library/std/src/panic.rs:359:14
    69:        0x105ef2334 - generator::detail::gen::catch_unwind_filter::h6e245238e49beb13
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/generator-0.8.8/src/detail/gen.rs:23:5
    70:        0x105ef24a8 - generator::detail::gen::gen_init_impl::hddf8ff605a2bae97
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/generator-0.8.8/src/detail/gen.rs:50:25
    71:        0x105ef21ec - generator::detail::asm::gen_init::h79e2768e93838269
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/generator-0.8.8/src/detail/aarch64_unix.rs:18:5
  failing seed:
  "
  13118085742490785141
  "
  To replay the failure, either:
      1) pass the seed to `shuttle::check_random_with_seed, or
      2) set the environment variable SHUTTLE_RANDOM_SEED to the seed and run `shuttle::check_random`.


  failures:
      shuttle_test_lost_updates

  test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.62s

  error: test failed, to rerun pass `-p turso_stress --test lost_updates`
  FAILED
  ➜  limbo git:(shuttle-mvcc) ✗ while RUST_BACKTRACE=full RUSTFLAGS="--cfg shuttle" cargo test -p turso_stress --test lost_updates; do
    echo "Pass, running again..."
  done
  echo "FAILED"
      Finished `test` profile [unoptimized + debuginfo] target(s) in 0.44s
       Running tests/lost_updates.rs (target/debug/deps/lost_updates-b986d5829159c088)

  running 2 tests
  test shuttle_test_lost_updates ... FAILED
  test shuttle_test_lost_updates_slow ... ok

  failures:

  ---- shuttle_test_lost_updates stdout ----
  WARNING: Shuttle only correctly models SeqCst atomics and treats all other Orderings as if they were SeqCst. Bugs caused by weaker orderings like Acquire may be missed. See
  https://docs.rs/shuttle/*/shuttle/sync/atomic/index.html#warning-about-relaxed-behaviors for details or to disable this warning.

  thread 'shuttle_test_lost_updates' panicked at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/shuttle-0.8.1/src/sync/atomic/mod.rs:234:41:
  already mutably borrowed: BorrowError
  stack backtrace:
     0:        0x10542ea00 - std::backtrace_rs::backtrace::libunwind::trace::h397b10519d455313
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/std/src/../../backtrace/src/backtrace/libunwind.rs:117:9
     1:        0x10542ea00 - std::backtrace_rs::backtrace::trace_unsynchronized::h902ac8b376515985
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/std/src/../../backtrace/src/backtrace/mod.rs:66:14
     2:        0x10542ea00 - std::sys::backtrace::_print_fmt::hd7805902c8299639
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/std/src/sys/backtrace.rs:66:9
     3:        0x10542ea00 - <std::sys::backtrace::BacktraceLock::print::DisplayBacktrace as core::fmt::Display>::fmt::h2c3f7b4c4ce00f07
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/std/src/sys/backtrace.rs:39:26
     4:        0x10544cee0 - core::fmt::rt::Argument::fmt::h1e6a80a47071126e
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/core/src/fmt/rt.rs:181:76
     5:        0x10544cee0 - core::fmt::write::h1dbafa36e52e01c5
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/core/src/fmt/mod.rs:1446:25
     6:        0x10542bd4c - std::io::default_write_fmt::h6bd5847749ac7605
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/std/src/io/mod.rs:639:11
     7:        0x10542bd4c - std::io::Write::write_fmt::hee580fb33ba5bfa5
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/std/src/io/mod.rs:1914:13
     8:        0x10542e8b4 - std::sys::backtrace::BacktraceLock::print::h1fb87370474572ed
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/std/src/sys/backtrace.rs:42:9
     9:        0x105430174 - std::panicking::default_hook::{{closure}}::h195a9b2c829547eb
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/std/src/panicking.rs:300:22
    10:        0x10542ff88 - std::panicking::default_hook::h18c3aa3e3a3584d5
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/std/src/panicking.rs:324:9
    11:        0x1043d0cb4 - <alloc::boxed::Box<F,A> as core::ops::function::Fn<Args>>::call::h7bed841a58f409e3
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/alloc/src/boxed.rs:1980:9
    12:        0x1043d0cb4 - test::test_main_with_exit_callback::{{closure}}::h29a05f78049a974e
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/test/src/lib.rs:145:21
    13:        0x1053f360c - <alloc::boxed::Box<F,A> as core::ops::function::Fn<Args>>::call::ha4648d6786b43557
                                 at /Users/avi/.rustup/toolchains/1.88.0-aarch64-apple-darwin/lib/rustlib/src/rust/library/alloc/src/boxed.rs:1980:9
    14:        0x1053b9c18 - shuttle::runtime::failure::init_panic_hook::{{closure}}::{{closure}}::hf30f57e0f21b98b2
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/shuttle-0.8.1/src/runtime/failure.rs:151:13
    15:        0x1053f360c - <alloc::boxed::Box<F,A> as core::ops::function::Fn<Args>>::call::ha4648d6786b43557
                                 at /Users/avi/.rustup/toolchains/1.88.0-aarch64-apple-darwin/lib/rustlib/src/rust/library/alloc/src/boxed.rs:1980:9
    16:        0x1053ee460 - generator::detail::gen::catch_unwind_filter::{{closure}}::{{closure}}::haa336ae7b7c732d8
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/generator-0.8.8/src/detail/gen.rs:19:13
    17:        0x105430c90 - <alloc::boxed::Box<F,A> as core::ops::function::Fn<Args>>::call::h9e486227024a0357
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/alloc/src/boxed.rs:1980:9
    18:        0x105430c90 - std::panicking::rust_panic_with_hook::h02a9fa3cad928562
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/std/src/panicking.rs:841:13
    19:        0x105430880 - std::panicking::begin_panic_handler::{{closure}}::hd1cc56578f819958
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/std/src/panicking.rs:706:13
    20:        0x10542eec4 - std::sys::backtrace::__rust_end_short_backtrace::h52c1e479035e4bc4
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/std/src/sys/backtrace.rs:168:18
    21:        0x105430528 - __rustc[4794b31dd7191200]::rust_begin_unwind
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/std/src/panicking.rs:697:5
    22:        0x1054955f4 - core::panicking::panic_fmt::heec96bfc27e6c546
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/core/src/panicking.rs:75:14
    23:        0x105495570 - core::cell::panic_already_mutably_borrowed::h8ed64df0236bfa1c
                                 at /rustc/6b00bc3880198600130e1cf62b8f8a93494488cc/library/core/src/cell.rs:799:5
    24:        0x1053dfc54 - core::cell::RefCell<T>::borrow::h9d46f12f4a75ab7e
                                 at /Users/avi/.rustup/toolchains/1.88.0-aarch64-apple-darwin/lib/rustlib/src/rust/library/core/src/cell.rs:987:25
    25:        0x1053a134c - shuttle::sync::atomic::Atomic<T>::exhale_clock::{{closure}}::h8710e42dad9901f4
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/shuttle-0.8.1/src/sync/atomic/mod.rs:234:30
    26:        0x105394d34 - shuttle::runtime::execution::ExecutionState::try_with::{{closure}}::hf223c0f1de6d2c21
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/shuttle-0.8.1/src/runtime/execution.rs:329:26
    27:        0x1053adc78 - scoped_tls::ScopedKey<T>::with::he0d59374e2ab4b96
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/scoped-tls-1.0.1/src/lib.rs:171:13
    28:        0x10538f014 - shuttle::runtime::execution::ExecutionState::try_with::h33d4b5a6787eec1f
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/shuttle-0.8.1/src/runtime/execution.rs:327:13
    29:        0x10538e3f4 - shuttle::runtime::execution::ExecutionState::with::h508c2a1a6e6a9d43
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/shuttle-0.8.1/src/runtime/execution.rs:316:9
    30:        0x1053a10ec - shuttle::sync::atomic::Atomic<T>::exhale_clock::hbe38b8896c7c1813
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/shuttle-0.8.1/src/sync/atomic/mod.rs:233:9
    31:        0x10539e5bc - shuttle::sync::atomic::Atomic<T>::fetch_update::h186a7db36f83f333
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/shuttle-0.8.1/src/sync/atomic/mod.rs:197:9
    32:        0x1053baaa4 - shuttle::sync::atomic::int::AtomicU32::fetch_update::h4bedc13421d8c82d
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/shuttle-0.8.1/src/sync/atomic/int.rs:74:17
    33:        0x1053baad8 - shuttle::sync::atomic::int::AtomicU32::fetch_add::h814e6041cf47523e
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/shuttle-0.8.1/src/sync/atomic/int.rs:128:17
    34:        0x104eb7240 - turso_core::storage::buffer_pool::Arena::new::{{closure}}::hd5fad8f77be93093
                                 at /Users/avi/turso/limbo/core/storage/buffer_pool.rs:395:31
    35:        0x104e2f958 - core::result::Result<T,E>::unwrap_or_else::he1e98efe10c442e4
                                 at /Users/avi/.rustup/toolchains/1.88.0-aarch64-apple-darwin/lib/rustlib/src/rust/library/core/src/result.rs:1491:23
    36:        0x1048fff50 - turso_core::storage::buffer_pool::Arena::new::he33fa9840c94151c
                                 at /Users/avi/turso/limbo/core/storage/buffer_pool.rs:391:18
    37:        0x1048fe8d8 - turso_core::storage::buffer_pool::PoolInner::init_arenas::he22f5bfb9e58ba46
                                 at /Users/avi/turso/limbo/core/storage/buffer_pool.rs:295:15
    38:        0x1048fe2b0 - turso_core::storage::buffer_pool::BufferPool::finalize_with_page_size::h4392a7fa931adb56
                                 at /Users/avi/turso/limbo/core/storage/buffer_pool.rs:233:13
    39:        0x1047b86dc - turso_core::storage::pager::Pager::allocate_page1::h28c36bcc0deadfc0
                                 at /Users/avi/turso/limbo/core/storage/pager.rs:4349:17
    40:        0x1047a53d8 - turso_core::storage::pager::Pager::maybe_allocate_page1::h725d81eaaf059bad
                                 at /Users/avi/turso/limbo/core/storage/pager.rs:2538:27
    41:        0x104b0f5c8 - turso_core::storage::pager::Pager::begin_write_tx::hb9550465ef73c663
                                 at /Users/avi/turso/limbo/core/storage/pager.rs:2551:23
    42:        0x104b0f5c8 - turso_core::vdbe::execute::op_transaction_inner::hf5324e88dc7a8a1a
                                 at /Users/avi/turso/limbo/core/vdbe/execute.rs:2650:46
    43:        0x104b0b8b8 - turso_core::vdbe::execute::op_transaction::h422c04ba84afefc9
                                 at /Users/avi/turso/limbo/core/vdbe/execute.rs:2354:18
    44:        0x104e7291c - turso_core::vdbe::Program::normal_step::h0ac42d6172ce5344
                                 at /Users/avi/turso/limbo/core/vdbe/mod.rs:1221:19
    45:        0x104e6977c - turso_core::vdbe::Program::step::hb3bb33d223f1a694
                                 at /Users/avi/turso/limbo/core/vdbe/mod.rs:1000:34
    46:        0x1048e7080 - turso_core::statement::Statement::_step::hf8b095867c4261e9
                                 at /Users/avi/turso/limbo/core/statement.rs:164:13
    47:        0x1048e7bcc - turso_core::statement::Statement::step_with_waker::hb3057a705b58fc8a
                                 at /Users/avi/turso/limbo/core/statement.rs:236:9
    48:        0x1043f6e84 - turso_sdk_kit::rsapi::TursoStatement::step_no_guard::h6627d6327522d945
                                 at /Users/avi/turso/limbo/sdk-kit/src/rsapi.rs:1009:17
    49:        0x1043f6dc0 - turso_sdk_kit::rsapi::TursoStatement::step::hb0a485adbb93731a
                                 at /Users/avi/turso/limbo/sdk-kit/src/rsapi.rs:1002:9
    50:        0x1043dad54 - turso::Statement::step::hb7eed40262822426
                                 at /Users/avi/turso/limbo/bindings/rust/src/lib.rs:321:15
    51:        0x1043da92c - <turso::Execute as core::future::future::Future>::poll::h1199d14527188d21
                                 at /Users/avi/turso/limbo/bindings/rust/src/lib.rs:304:15
    52:        0x104382cf8 - turso::Statement::execute::{{closure}}::he5c421fd97b01ca1
                                 at /Users/avi/turso/limbo/bindings/rust/src/lib.rs:395:17
    53:        0x10439e138 - turso::connection::Connection::execute::{{closure}}::hc31ee9cadb13bd62
                                 at /Users/avi/turso/limbo/bindings/rust/src/connection.rs:118:30
    54:        0x10439247c - lost_updates::setup_mvcc_db::{{closure}}::h8294357029803f64
                                 at /Users/avi/turso/limbo/testing/stress/tests/lost_updates.rs:29:49
    55:        0x10439335c - lost_updates::lost_updates_scenario::{{closure}}::hdfebf0802314869a
                                 at /Users/avi/turso/limbo/testing/stress/tests/lost_updates.rs:53:6
    56:        0x10438cee4 - shuttle::future::block_on::h2db26edd5d7e93f9
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/shuttle-0.8.1/src/future/mod.rs:243:15
    57:        0x104394704 - lost_updates::shuttle_test_lost_updates::{{closure}}::h882e85d3357d0546
                                 at /Users/avi/turso/limbo/testing/stress/tests/lost_updates.rs:107:19
    58:        0x104386138 - shuttle::runtime::runner::Runner<S>::run::{{closure}}::{{closure}}::{{closure}}::h1b96c3d78eb57c93
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/shuttle-0.8.1/src/runtime/runner.rs:106:101
    59:        0x1043a11b4 - shuttle::thread::thread_fn::hafab832c82fe35e6
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/shuttle-0.8.1/src/thread.rs:201:15
    60:        0x10438ad3c - shuttle::runtime::execution::Execution::run::{{closure}}::{{closure}}::h50fd4b765cc29bfb
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/shuttle-0.8.1/src/runtime/execution.rs:83:34
    61:        0x104396fbc - core::ops::function::FnOnce::call_once{{vtable.shim}}::hdb09b09bb7be02e8
                                 at /Users/avi/.rustup/toolchains/1.88.0-aarch64-apple-darwin/lib/rustlib/src/rust/library/core/src/ops/function.rs:250:5
    62:        0x1053eb6d4 - <alloc::boxed::Box<F,A> as core::ops::function::FnOnce<Args>>::call_once::h1163b76569e97a04
                                 at /Users/avi/.rustup/toolchains/1.88.0-aarch64-apple-darwin/lib/rustlib/src/rust/library/alloc/src/boxed.rs:1966:9
    63:        0x1053c2544 - shuttle::runtime::thread::continuation::Continuation::new::{{closure}}::hc043a252e52f3f9f
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/shuttle-0.8.1/src/runtime/thread/continuation.rs:87:21
    64:        0x1053cd0f8 - generator::gen_impl::GeneratorImpl<A,T>::init_code::{{closure}}::h1f64ec2622e53e8c
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/generator-0.8.8/src/gen_impl.rs:357:21
    65:        0x1053c16b0 - generator::stack::StackBox<F>::call_once::hfc88481d9a704768
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/generator-0.8.8/src/stack/mod.rs:137:13
    66:        0x1053ef210 - generator::stack::Func::call_once::h39573e5627669fa8
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/generator-0.8.8/src/stack/mod.rs:119:9
    67:        0x1053ee5e4 - generator::detail::gen::gen_init_impl::{{closure}}::hac9963fc0dd568b1
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/generator-0.8.8/src/detail/gen.rs:35:9
    68:        0x1053f1160 - core::ops::function::FnOnce::call_once::hfbe2ad52aced7712
                                 at /Users/avi/.rustup/toolchains/1.88.0-aarch64-apple-darwin/lib/rustlib/src/rust/library/core/src/ops/function.rs:250:5
    69:        0x1053f0ad0 - std::panicking::try::do_call::h1dc989a35d7b4911
                                 at /Users/avi/.rustup/toolchains/1.88.0-aarch64-apple-darwin/lib/rustlib/src/rust/library/std/src/panicking.rs:589:40
    70:        0x1053f3bd4 - ___rust_try
    71:        0x1053f0a38 - std::panicking::try::hbaa2cb079860786a
                                 at /Users/avi/.rustup/toolchains/1.88.0-aarch64-apple-darwin/lib/rustlib/src/rust/library/std/src/panicking.rs:552:19
    72:        0x1053f0a38 - std::panic::catch_unwind::h983cb91582a67f64
                                 at /Users/avi/.rustup/toolchains/1.88.0-aarch64-apple-darwin/lib/rustlib/src/rust/library/std/src/panic.rs:359:14
    73:        0x1053ee334 - generator::detail::gen::catch_unwind_filter::h6e245238e49beb13
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/generator-0.8.8/src/detail/gen.rs:23:5
    74:        0x1053ee4a8 - generator::detail::gen::gen_init_impl::hddf8ff605a2bae97
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/generator-0.8.8/src/detail/gen.rs:50:25
    75:        0x1053ee1ec - generator::detail::asm::gen_init::h79e2768e93838269
                                 at /Users/avi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/generator-0.8.8/src/detail/aarch64_unix.rs:18:5
