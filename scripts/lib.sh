#!/bin/bash
#
# Shared by the P2 scripts. `BUILT_PRODUCTS_DIR` was extracted by the same
# `xcodebuild -showBuildSettings | awk` pipeline in three places.

SCHEME=${SCHEME:-NADE}
PROJECT=${PROJECT:-NADE.xcodeproj}
DESTINATION=${DESTINATION:-'platform=iOS Simulator,name=iPhone 17 Pro'}

products_dir() {  # configuration
    xcodebuild -project "$PROJECT" -scheme "$SCHEME" \
        -configuration "$1" -sdk iphonesimulator \
        -destination "$DESTINATION" -showBuildSettings 2>/dev/null \
        | awk -F' = ' '/ BUILT_PRODUCTS_DIR/ { print $2; exit }'
}
