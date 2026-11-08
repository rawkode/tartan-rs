// Package types defines the typed properties that Tartan extensions store and
// that repositories configure: the schema each property's definition must fit,
// including the `default` value it may carry.
//
// Every property has a `kind`, an optional `label` (the title people see) and
// an optional `name`. An extension that keeps properties by name, such as
// Issues, sets `name` to the key it is stored under, so another extension's
// configuration can refer to the property itself rather than repeat it.
package types

import (
	"list"
	"strings"
	"time"
)

// The name of a property: a lowercase identifier, usable as a JSON key.
#Name: =~"^[a-z][a-z0-9_]{0,31}$"

// What every property carries.
#Common: {
	label?: string & strings.MinRunes(1) & strings.MaxRunes(100)
	name?:  #Name
}

// The longest short text, in characters.
#ShortTextMax: 200

// The longest long text, in characters.
#LongTextMax: 20000

// The largest amount of money in minor units: what a JSON number keeps exact.
#MoneyMax: 9007199254740991

// A one-line text of at most `maxLength` characters (200 at most).
#ShortText: {
	#Common
	kind:       "shorttext"
	maxLength?: int & >=1 & <=#ShortTextMax
	default?:   string & !~"[\r\n]" & strings.MaxRunes(#ShortTextMax)
	if maxLength != _|_ if default != _|_ {
		default: strings.MaxRunes(maxLength)
	}
}

// Markdown of at most `maxLength` characters (20000 at most).
#LongText: {
	#Common
	kind:       "longtext"
	maxLength?: int & >=1 & <=#LongTextMax
	default?:   string & strings.MaxRunes(#LongTextMax)
	if maxLength != _|_ if default != _|_ {
		default: strings.MaxRunes(maxLength)
	}
}

// A number between optional bounds.
#Number: {
	#Common
	kind:     "number"
	min?:     number
	max?:     number
	default?: number
	if min != _|_ if max != _|_ {
		max: >=min
	}
	if min != _|_ if default != _|_ {
		default: >=min
	}
	if max != _|_ if default != _|_ {
		default: <=max
	}
}

// A day on the calendar, written `YYYY-MM-DD`.
#Date: {
	#Common
	kind:     "date"
	default?: #DateValue
}

// `YYYY-MM-DD`, a real day.
#DateValue: =~"^[0-9]{4}-[0-9]{2}-[0-9]{2}$" & time.Format("2006-01-02")

// Yes or no.
#Boolean: {
	#Common
	kind:     "boolean"
	default?: bool
}

// The options of a select or multi-select: 1 to 100 distinct, non-empty
// strings of at most 100 characters.
#Options: [string, ...string] & list.UniqueItems() & list.MaxItems(100) & [...(string & strings.MinRunes(1) & strings.MaxRunes(100))]

// One of a list of options. `labels` gives an option the title people see.
#Select: {
	#Common
	kind:    "select"
	options: #Options
	labels?: [or(options)]: string & strings.MinRunes(1) & strings.MaxRunes(100)
	default?: or(options)
}

// Any distinct options of a list. `labels` gives an option the title people see.
#MultiSelect: {
	#Common
	kind:    "multiselect"
	options: #Options
	labels?: [or(options)]: string & strings.MinRunes(1) & strings.MaxRunes(100)
	default?: [...or(options)] & list.UniqueItems()
}

// A principal, by its id (such as `usr_dev`).
#Person: {
	#Common
	kind:     "person"
	default?: #PersonValue
}

// A principal id: 1 to 128 letters, digits and `._:@-`, starting with a
// letter or digit.
#PersonValue: =~"^[A-Za-z0-9][A-Za-z0-9._:@-]{0,127}$"

// An `http` or `https` URL.
#URL: {
	#Common
	kind:     "url"
	default?: #URLValue
}

// An absolute `http` or `https` URL of at most 2048 characters, without
// whitespace.
#URLValue: =~"^https?://[^\\s/?#]+[^\\s]*$" & strings.MaxRunes(2048)

// An amount of money in one currency. Values are whole minor units
// (1250 is GBP 12.50), so amounts stay exact.
#Money: {
	#Common
	kind:     "money"
	currency: =~"^[A-Z]{3}$"
	default?: int & >=-#MoneyMax & <=#MoneyMax
}

// Any property.
#Property: #ShortText | #LongText | #Number | #Date | #Boolean | #Select | #MultiSelect | #Person | #URL | #Money
