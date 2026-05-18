import Image from "next/image";

export function BrandMark({ className = "h-9 w-9" }: { className?: string }) {
  return (
    <Image
      src="/brand/grokrxiv-mark.svg"
      alt=""
      aria-hidden="true"
      className={className}
      width={32}
      height={32}
      unoptimized
    />
  );
}
