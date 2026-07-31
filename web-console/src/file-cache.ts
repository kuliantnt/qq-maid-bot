export const CACHE_NAME = "console-user-files-v1";

/** 自定义背景文件的读取走 POST；Cache API 匹配时使用相同方法，避免与 GET 语义混淆。 */
export function fileCacheRequest(url: string): Request {
  return new Request(url, { method: "POST" });
}

export async function cacheFileBlob(url: string, blob: Blob): Promise<boolean> {
  if (typeof caches === "undefined") return false;
  try {
    const cache = await caches.open(CACHE_NAME);
    await cache.put(fileCacheRequest(url), new Response(blob, {
      headers: { "Content-Type": blob.type || "application/octet-stream" },
    }));
    return true;
  } catch (cause) {
    return false;
  }
}

export async function readCachedFileBlob(url: string): Promise<Blob | null> {
  if (typeof caches === "undefined") return null;
  try {
    const cache = await caches.open(CACHE_NAME);
    const response = await cache.match(fileCacheRequest(url));
    return response === undefined ? null : await response.blob();
  } catch (cause) {
    return null;
  }
}

/** 删除指定文件条目；条目不存在也算删除成功，仅在 Cache API 失败时返回 false。 */
export async function deleteCachedFileBlob(url: string): Promise<boolean> {
  if (typeof caches === "undefined") return false;
  try {
    const cache = await caches.open(CACHE_NAME);
    await cache.delete(fileCacheRequest(url));
    return true;
  } catch (cause) {
    return false;
  }
}

export async function clearFileBlobCache(): Promise<boolean> {
  if (typeof caches === "undefined") return false;
  try {
    return await caches.delete(CACHE_NAME);
  } catch (cause) {
    return false;
  }
}
