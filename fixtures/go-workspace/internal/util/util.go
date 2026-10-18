// Package util holds small helpers shared inside the module.
package util

import (
	"fmt"
	"strings"
)

// Cents formats an amount in cents as a decimal string.
func Cents(v int64) string {
	sign := ""
	if v < 0 {
		sign, v = "-", -v
	}
	return fmt.Sprintf("%s%d.%02d", sign, v/100, v%100)
}

// Fold lower-cases and trims a string for comparisons.
func Fold(s string) string {
	return strings.ToLower(strings.TrimSpace(s))
}
