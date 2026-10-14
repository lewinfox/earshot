# Everyday commands. `make help` lists them.

PACKAGES := earshot-gui earshot-engine earshot-audio

.PHONY: help run release test test-worker fmt clean clean-all clean-db bundle

help:
	@echo "make run          run the app (debug build)"
	@echo "make release      run the app (release build)"
	@echo "make test         Rust tests, whole workspace"
	@echo "make test-worker  the Python worker's tests"
	@echo "make fmt          format the Rust code"
	@echo "make clean        delete earshot's own build outputs, keep the compiled libraries"
	@echo "make clean-all    delete everything in target/ (next build is from scratch)"
	@echo "make clean-db     delete the library database (after a schema change)"
	@echo "make bundle       a Linux release folder, ready to run (target/bundle/earshot)"

run:
	cargo run -p earshot-gui

release:
	cargo run --release -p earshot-gui

test:
	cargo test --workspace -- --test-threads=4

test-worker:
	cd worker && .venv/bin/python -m pytest -q

fmt:
	cargo fmt

# What a release ships (README "Releases"), checked with its own Python.
bundle:
	cargo build --release --locked -p earshot-gui
	packaging/bundle.sh linux-x86_64 target/release/earshot-gui target/bundle/earshot
	target/bundle/earshot/python/bin/python3 packaging/smoke_test.py target/bundle/earshot

# Every build of our crates with different settings leaves its own ~400 MB binary,
# and cargo never deletes old ones. The libraries we depend on rarely change, so
# keeping them makes the next build quick.
clean:
	cargo clean $(addprefix -p ,$(PACKAGES))
	cargo clean --release $(addprefix -p ,$(PACKAGES))

clean-all:
	cargo clean

# The data folder, as core/services.rs `data_dir` finds it.
ifeq ($(shell uname),Darwin)
DATA_DIR ?= $(or $(EARSHOT_DATA_DIR),$(HOME)/Library/Application Support/io.github.lewinfox.earshot)
else
DATA_DIR ?= $(or $(EARSHOT_DATA_DIR),$(or $(XDG_DATA_HOME),$(HOME)/.local/share)/io.github.lewinfox.earshot)
endif

# The database (tracks, analysis, playlists and the settings kept in it) and its
# write-ahead log. Logs, the keymap file and the UI layout stay.
clean-db:
	rm -f "$(DATA_DIR)/library.db" "$(DATA_DIR)/library.db-wal" "$(DATA_DIR)/library.db-shm"
