import { cn } from "@charlotte-ui/utils"

type LogoName = "brand" | "dispatch" | "faber" | "herald" | "naos" | "strophe"
type ServiceName = Exclude<LogoName, "brand">

type LogoProps = Omit<React.ComponentProps<"img">, "alt" | "height" | "src" | "width"> & {
  size?: number
}

const ASSET_BASE_URL = "https://ui.registry.panit.dev/assets"

type LogoSource = {
  alt: string
  src: string
  srcSet: string
}

function createLogoSource(name: string, alt: string): LogoSource {
  return {
    alt,
    src: `${ASSET_BASE_URL}/${name}-28.png`,
    srcSet: [
      `${ASSET_BASE_URL}/${name}-28.png 1x`,
      `${ASSET_BASE_URL}/${name}-56.png 2x`,
      `${ASSET_BASE_URL}/${name}-84.png 3x`,
    ].join(", "),
  }
}

const logoSources: Record<LogoName, LogoSource> = {
  brand: {
    ...createLogoSource("logo", "Panit"),
  },
  dispatch: createLogoSource("dispatch-logo", "Dispatch"),
  faber: createLogoSource("faber-logo", "Faber"),
  herald: createLogoSource("herald-logo", "Herald"),
  naos: createLogoSource("naos-logo", "Naos"),
  strophe: createLogoSource("strophe-logo", "Strophe"),
}

function Logo({
  className,
  name,
  size = 32,
  ...props
}: LogoProps & { name: LogoName }) {
  const logo = logoSources[name]

  return (
    <img
      alt={logo.alt}
      className={cn("shrink-0 object-contain", className)}
      height={size}
      src={logo.src}
      srcSet={logo.srcSet}
      width={size}
      {...props}
    />
  )
}

export function BrandLogo(props: LogoProps) {
  return <Logo name="brand" {...props} />
}

export function DispatchLogo(props: LogoProps) {
  return <Logo name="dispatch" {...props} />
}

export function HeraldLogo(props: LogoProps) {
  return <Logo name="herald" {...props} />
}

export function FaberLogo(props: LogoProps) {
  return <Logo name="faber" {...props} />
}

export function NaosLogo(props: LogoProps) {
  return <Logo name="naos" {...props} />
}

export function StropheLogo(props: LogoProps) {
  return <Logo name="strophe" {...props} />
}

export function ServiceLogo({
  service,
  ...props
}: LogoProps & { service: ServiceName }) {
  return <Logo name={service} {...props} />
}
