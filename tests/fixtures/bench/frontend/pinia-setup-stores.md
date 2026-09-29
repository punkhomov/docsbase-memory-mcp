# Pinia Stores

Pinia is the store library for Vue. It supports option stores and setup stores.

## Setup store syntax

The setup store syntax passes a function to `defineStore`:

```js
export const useCounterStore = defineStore('counter', () => {
  const count = ref(0)
  const double = computed(() => count.value * 2)
  function increment() { count.value++ }
  return { count, double, increment }
})
```

`defineStore` in setup form receives the store id and a setup function whose
returned refs become state and whose functions become actions.
