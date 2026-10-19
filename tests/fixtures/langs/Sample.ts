export interface A extends B, C { m(): void }
enum E { X }
const enum F { Y }
export type Props = { id: string };
abstract class S implements A { abstract m(): void; static of() {} get size() { return 0 } #secret() {} handle = () => this.of2(); of2() {} }
function f(a: string): void;
function f(a: number): void;
function f(a: any) {}
namespace N { export function g() {} export class K {} }
