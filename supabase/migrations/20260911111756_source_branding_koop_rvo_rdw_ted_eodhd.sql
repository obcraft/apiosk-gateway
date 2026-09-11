-- Official source artwork hosted with the website; preserve all other listing metadata.
UPDATE public.apis AS a
SET listing_metadata = coalesce(a.listing_metadata, '{}'::jsonb) || jsonb_build_object('logo', b.logo)
FROM (VALUES
 ('koop', 'https://apiosk.com/brands/koop.png'),
 ('rvo', 'https://apiosk.com/brands/rvo.svg'),
 ('rdw', 'https://apiosk.com/brands/rdw.svg'),
 ('ted', 'https://apiosk.com/brands/ted.png'),
 ('eodhd', 'https://apiosk.com/brands/eodhd.svg')
) AS b(slug, logo)
WHERE a.slug = b.slug;
