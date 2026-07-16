# Atlas

The unified graph, read-only, in five architecture bands (Infrastructure →
Cloud → Server → Events → Client) at deterministic positions. Past a size
threshold the initial view collapses per-band clusters that expand on demand.

- **Focus mode**: select a node and press **Enter** to re-root the view on it
  and its direct connections; **Esc** backs out one level; the breadcrumb
  shows the focus path.
- The **confidence overlay** colors facts by confidence; shapes encode kind
  (octagon = gap, diamond = channel) so color never carries alone.
- Every node and edge opens read-only evidence.
