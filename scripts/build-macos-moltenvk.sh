#!/usr/bin/env bash
# Pinned MoltenVK with the upstream SPIRV-Cross Metal type-name fix.
set -euo pipefail
destination=${1:?Usage: build-macos-moltenvk.sh CACHE_DIRECTORY}
molten_revision=db66022459ffb663aa2b50f6b018bc2e124f5edf
cross_revision=cd3fcb2603ede297edb90ab5a679e4ac814055e2
cache="$destination/$molten_revision-$cross_revision"
if [[ -f "$cache/complete" ]]; then
  (cd "$cache" && shasum -a 256 -c complete >&2)
  printf '%s\n' "$cache"
  exit 0
fi
mkdir -p "$destination"
temporary=$(mktemp -d "$destination/.build.XXXXXX")
trap 'rm -rf "$temporary"' EXIT
temporary=$(cd "$temporary" && pwd)
checkout() {
  local path=$1 repository=$2 revision=$3
  git init -q "$path"
  git -C "$path" remote add origin "https://github.com/$repository.git"
  git -C "$path" fetch -q --depth 1 origin "$revision"
  git -C "$path" checkout -q --detach FETCH_HEAD
}
checkout "$temporary/source" KhronosGroup/MoltenVK "$molten_revision"
source_dir="$temporary/source"
printf '%s\n' "$cross_revision" > "$source_dir/ExternalRevisions/SPIRV-Cross_repo_revision"
# Seed shallow checkouts; upstream's fetcher otherwise downloads full histories.
for specification in \
  'cereal USCiLab/cereal cereal' \
  'Vulkan-Headers KhronosGroup/Vulkan-Headers Vulkan-Headers' \
  'SPIRV-Cross KhronosGroup/SPIRV-Cross SPIRV-Cross' \
  'SPIRV-Tools KhronosGroup/SPIRV-Tools SPIRV-Tools' \
  'SPIRV-Tools/external/spirv-headers KhronosGroup/SPIRV-Headers SPIRV-Headers' \
  'Vulkan-Tools KhronosGroup/Vulkan-Tools Vulkan-Tools' \
  'Volk zeux/Volk Volk'; do
  read -r directory repository revision_file <<< "$specification"
  checkout "$source_dir/External/$directory" "$repository" "$(cat "$source_dir/ExternalRevisions/${revision_file}_repo_revision")"
done
(
  cd "$source_dir"
  ./fetchDependencies --macos
  xcodebuild build -project MoltenVKPackaging.xcodeproj \
    -scheme 'MoltenVK Package (macOS only)' -destination 'generic/platform=macOS' \
    -configuration Release -derivedDataPath "$temporary/derived" -jobs 2 \
    ARCHS=arm64 ONLY_ACTIVE_ARCH=YES CODE_SIGNING_ALLOWED=NO
) >&2
mkdir "$temporary/result"
cp "$source_dir/Package/Release/MoltenVK/dynamic/dylib/macOS/libMoltenVK.dylib" "$temporary/result/"
cp "$source_dir/LICENSE" "$temporary/result/MoltenVK.LICENSE"
cp "$source_dir/External/SPIRV-Cross/LICENSE" "$temporary/result/SPIRV-Cross.LICENSE"
(cd "$temporary/result" && shasum -a 256 libMoltenVK.dylib *.LICENSE > complete)
mv "$temporary/result" "$cache"
printf '%s\n' "$cache"
