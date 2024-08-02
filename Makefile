.PHONY: default

default: vulcan

vulcan: **.go
	go build -tags 'sqlite_vtable' .
