package raptor

// OrderedMap is a string-keyed map that preserves insertion order on
// iteration, replicating JS object `Object.keys` / `for..in` semantics for the
// non-integer string keys that RAPTOR uses (stop IDs, route IDs).
//
// Go's built-in map iteration is randomized, so this type is used everywhere
// the reference relies on insertion-ordered iteration.
type OrderedMap[V any] struct {
	keys   []string
	values map[string]V
}

// NewOrderedMap constructs an empty OrderedMap.
func NewOrderedMap[V any]() *OrderedMap[V] {
	return &OrderedMap[V]{values: map[string]V{}}
}

// Set inserts or updates a key, recording first-insertion order.
func (m *OrderedMap[V]) Set(k string, v V) {
	if _, ok := m.values[k]; !ok {
		m.keys = append(m.keys, k)
	}
	m.values[k] = v
}

// Get returns the value and whether the key is present.
func (m *OrderedMap[V]) Get(k string) (V, bool) {
	v, ok := m.values[k]
	return v, ok
}

// Has reports whether the key is present.
func (m *OrderedMap[V]) Has(k string) bool {
	_, ok := m.values[k]
	return ok
}

// GetOr returns the value or a fallback if absent.
func (m *OrderedMap[V]) GetOr(k string, fallback V) V {
	if v, ok := m.values[k]; ok {
		return v
	}
	return fallback
}

// Keys returns the keys in insertion order (a copy is not made; do not mutate).
func (m *OrderedMap[V]) Keys() []string {
	return m.keys
}

// Len returns the number of entries.
func (m *OrderedMap[V]) Len() int {
	return len(m.keys)
}
