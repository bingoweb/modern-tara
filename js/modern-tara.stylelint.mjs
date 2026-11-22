// stylelint config owned by modern-tara. Core rules flag old forms (kind
// "modern"); plugin/use-baseline flags features the target browsers may
// lack (kind "baseline"). stylelint 17 has no core rule for logical
// properties, so none is listed. Style-only rules are left out because they do
// not move code to a newer construct: alpha-value-notation (number vs. percent)
// and shorthand-property-no-redundant-values (redundancy).
// declaration-block-no-redundant-longhand-properties stays: modern-tara keeps
// its findings only for shorthands newer than their longhands (inset, gap,
// place-*, margin-block, ...), with low confidence.

export default function config(useBaseline, available = 'widely') {
  return {
    plugins: [useBaseline],
    rules: {
      'color-function-notation': 'modern',
      'media-feature-range-notation': 'context',
      'declaration-block-no-redundant-longhand-properties': true,
      'property-no-vendor-prefix': true,
      'value-no-vendor-prefix': true,
      'selector-no-vendor-prefix': true,
      'at-rule-no-vendor-prefix': true,
      'media-feature-name-no-vendor-prefix': true,
      'plugin/use-baseline': [true, { available }],
    },
  };
}
