export interface Box { inner?: { value: number } }

export function chained(box: Box | undefined): number | undefined {
  return box && box.inner && box.inner.value; // OLD:prefer-optional-chain:modern
}

export function modern(box: Box | undefined): number | undefined {
  return box?.inner?.value; // DECOY
}
