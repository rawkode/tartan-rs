package test

import "tartan.dev/types"

p: types.#LongText & {label: "Notes", maxLength: 5000, default: "# Notes\n\n- one"}
