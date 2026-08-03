EXTENSION := pgcontext
PACKAGE := context-pg
PG_CONFIG ?= pg_config
PGRX ?= cargo pgrx
TAG ?= v0.2.0
PG_MAJOR := $(shell $(PG_CONFIG) --version 2>/dev/null | sed -E 's/[^0-9]*([0-9]+).*/\1/')
PG_FEATURE := pg$(PG_MAJOR)

.PHONY: all check-supported-pg install install-pgcontext-upgrades installcheck package quickstart clean

all: package

check-supported-pg:
	@test "$(PG_MAJOR)" = "17" -o "$(PG_MAJOR)" = "18" || { \
		echo "pgContext source launch supports PostgreSQL 17 and 18; selected $(PG_MAJOR)" >&2; \
		exit 1; \
	}

install: check-supported-pg
	$(PGRX) install -p $(PACKAGE) --pg-config $(PG_CONFIG) --release \
		--no-default-features --features $(PG_FEATURE)
	$(MAKE) install-pgcontext-upgrades

install-pgcontext-upgrades: check-supported-pg
	scripts/install-pgcontext-upgrades.sh $(PG_CONFIG)

installcheck: check-supported-pg
	$(PGRX) test -p $(PACKAGE) $(PG_FEATURE)

package:
	release/build-packages.sh $(TAG)

quickstart:
	scripts/quickstart.sh

clean:
	cargo clean
	rm -rf dist
