import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

/** 合并 Tailwind class；语义 token 类名与结构类名冲突时后者覆盖前者。 */
export function cn(...inputs: ClassValue[]): string {
  return twMerge(clsx(inputs));
}
