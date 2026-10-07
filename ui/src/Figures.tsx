// Thumbnails of extracted figures; each opens the reader where the figure appears.
import { href } from "./App";
import type { ImageRef } from "./api";
import { useT } from "./i18n";
import { imageUrl } from "./markdown";

export default function Figures({ project, images, label }: { project: string; images: ImageRef[]; label?: string }) {
  const t = useT();
  if (!images.length) return null;
  return (
    <ul className="figures" aria-label={label ?? t("figures.aria")}>
      {images.map((img) => {
        const caption = img.caption || t("figures.untitled", { doc: img.doc_title });
        const where: Record<string, string> = img.page ? { doc: img.doc_id, page: String(img.page) } : { doc: img.doc_id };
        return (
          <li key={`${img.chunk_id}-${img.file}`}>
            <a href={href("lire", where)} title={img.page ? `${caption} (${t("common.page", { n: img.page })})` : caption}>
              <img src={imageUrl(project, `innerrag-image:${img.file}`)} alt={caption} loading="lazy" />
              <span className="figures-caption">{caption}</span>
            </a>
          </li>
        );
      })}
    </ul>
  );
}
