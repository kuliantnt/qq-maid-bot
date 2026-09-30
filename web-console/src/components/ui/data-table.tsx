import type { ReactNode } from "react";
import { cn } from "../../lib/utils.js";

type DataTableProps = {
  columns: readonly string[];
  /** 每行单元格；空态时渲染 empty。 */
  rows: readonly (readonly ReactNode[])[];
  empty?: ReactNode;
  className?: string;
};

/**
 * 数据表原语：窄屏横向滚动容器由调用方决定是否启用；
 * 表头使用等宽小写字距样式，与控制台技术数据风格一致。
 */
export function DataTable({ columns, rows, empty, className }: DataTableProps) {
  if (rows.length === 0 && empty !== undefined) {
    return <p className="m-0 px-2 py-6 text-center text-sm text-muted">{empty}</p>;
  }
  return (
    <div className={cn("overflow-x-auto", className)}>
      <table className="w-full border-collapse text-sm">
        <thead>
          <tr>
            {columns.map((column) => (
              <th
                key={column}
                scope="col"
                className="border-b border-line px-2 py-2 text-left font-mono text-[0.66rem] font-bold tracking-widest text-muted uppercase whitespace-nowrap"
              >
                {column}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((row, rowIndex) => (
            <tr key={rowIndex} className="border-b border-line-inner last:border-b-0">
              {row.map((cellValue, cellIndex) => (
                <td key={cellIndex} className="px-2 py-2.5 align-middle whitespace-nowrap">
                  {cellValue}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
