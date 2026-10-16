import { getProviderIcon } from "./providerIcons";

interface Props {
  providerId: string;
  size?: number;
  className?: string;
  title?: string;
}

/**
 * Renders a provider brand icon. If the registry has an SVG, inline it inside
 * a rounded badge tinted with the brand color; otherwise render a circular
 * badge containing a fallback letter.
 */
export function ProviderIcon({
  providerId,
  size = 22,
  className,
  title,
}: Props) {
  const entry = getProviderIcon(providerId);
  const dims = { width: size, height: size };

  // Set both the CSS var and `color` so fill="currentColor" glyphs stay brand-
  // tinted even when a parent theme only forwards --brand.
  const brandStyle = {
    ...dims,
    color: entry.monochrome ? "var(--text-primary)" : entry.brandColor,
    ["--provider-brand" as string]: entry.brandColor,
  };

  if (entry.svgPath) {
    return (
      <span
        className={`provider-icon provider-icon--svg${className ? " " + className : ""}`}
        data-provider={entry.id}
        style={brandStyle}
        title={title}
        aria-hidden={title ? undefined : true}
        // eslint-disable-next-line react/no-danger -- SVGs are bundled locally, no user input.
        dangerouslySetInnerHTML={{ __html: entry.svgPath }}
      />
    );
  }

  return (
    <span
      className={`provider-icon provider-icon--letter${className ? " " + className : ""}`}
      data-provider={entry.id}
      style={brandStyle}
      title={title}
      aria-hidden={title ? undefined : true}
    >
      {entry.fallbackLetter}
    </span>
  );
}
