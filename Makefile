# Everyday commands. `make help` lists them.

PACKAGES := earshot-gui earshot-engine earshot-audio

.PHONY: help run release test test-worker fmt clean clean-all

help:
	@echo "make run          run the app (debug build)"
	@echo "make release      run the app (release build)"
	@echo "make test         Rust tests, whole workspace"
	@echo "make test-worker  the Python worker's tests"
	@echo "make fmt          format the Rust code"
	@echo "make clean        delete earshot's own build outputs, keep the compiled libraries"
	@echo "make clean-all    delete everything in target/ (next build is from scratch)"

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

# Every build of our crates with different settings leaves its own ~400 MB binary,
# and cargo never deletes old ones. The libraries we depend on rarely change, so
# keeping them makes the next build quick.
clean:
	cargo clean $(addprefix -p ,$(PACKAGES))
	cargo clean --release $(addprefix -p ,$(PACKAGES))

clean-all:
	cargo clean
