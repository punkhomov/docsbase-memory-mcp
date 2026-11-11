// One owner for cancellation and stale-result rejection, including project switches.
export class Latest {
  generation = 0;
  controller = null;
  cancel() {
    this.generation++;
    this.controller?.abort();
    this.controller = null;
  }
  start() {
    this.cancel();
    this.controller = new AbortController();
    return { generation: this.generation, signal: this.controller.signal };
  }
  current(generation) {
    return generation === this.generation;
  }
}
