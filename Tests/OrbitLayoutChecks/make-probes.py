from pathlib import Path
import sys

project = Path(__file__).resolve().parents[2]
src = (project / 'Sources/OrbitDisk/ContentView.swift').read_text()
def replace(a,b,n=1):
    global src
    assert a in src, a
    src=src.replace(a,b,n)
replace('ToolbarItem(placement: .navigation) { headerLocation }', 'ToolbarItem(placement: .navigation) { headerLocation.toolbarProbe("toolbar-location") }', 2)
replace('ToolbarItem(placement: .primaryAction) { header }', 'ToolbarItem(placement: .primaryAction) { header.toolbarProbe("toolbar-actions") }', 2)
replace('}.frame(width: 280, alignment: .leading)', '}.frame(width: 280, alignment: .leading).toolbarProbe("location")')
replace('                collectionBar\n','                collectionBar.layoutProbe("tray")\n')
replace('.buttonStyle(.plain).pointingCursor().padding(.horizontal, 12).padding(.bottom, 14)', '.buttonStyle(.plain).pointingCursor().padding(.horizontal, 12).padding(.bottom, 14).layoutProbe("device:\\(volume.id)")')
replace('}.padding(.horizontal, 28).frame(height: 48)','}.padding(.horizontal, 28).frame(height: 48).layoutProbe("breadcrumb")')
replace('.padding(.horizontal, 16).frame(maxWidth: .infinity, maxHeight: .infinity)','.padding(.horizontal, 16).frame(maxWidth: .infinity, maxHeight: .infinity).layoutProbe("map")')
replace('}.padding(.bottom, 12)','}.padding(.bottom, 12).layoutProbe("metrics")')
replace('filePanel(node).frame(width: 340)','filePanel(node).frame(width: 340).layoutProbe("panel")')
replace('            scanFooter\n','            scanFooter.layoutProbe("footer")\n')
replace('            inspector\n','            inspector.layoutProbe("inspector")\n')
replace('.padding(9).frame(width: 210, height: 34)','.padding(9).frame(width: 210, height: 34).toolbarProbe("search")')
replace('.pointingCursor().help("Refresh the full map (⌘R)").fixedSize()','.pointingCursor().help("Refresh the full map (⌘R)").fixedSize().toolbarProbe("refresh")')
replace('.pointingCursor().fixedSize()\n                .accessibilityLabel("Open Folder…")','.pointingCursor().fixedSize().toolbarProbe("open-folder")\n                .accessibilityLabel("Open Folder…")')
replace('.disabled(model.navigation.count < 2).help("Go up one folder (⌘[)")','.disabled(model.navigation.count < 2).help("Go up one folder (⌘[)").layoutProbe("up")')
replace('.controlSize(.regular).frame(height: 26).disabled(selected == nil)','.controlSize(.regular).frame(height: 26).layoutProbe("inspector-actions").disabled(selected == nil)')
replace('                .frame(height: 32)\n','                .frame(height: 32).layoutProbe("collect-action")\n')
replace('.pointingCursor().disabled(!model.canReviewDisplayedCollection || model.isRemoving || model.isValidatingCollection || model.isUpdatingCollection || dragging).fixedSize()','.pointingCursor().disabled(!model.canReviewDisplayedCollection || model.isRemoving || model.isValidatingCollection || model.isUpdatingCollection || dragging).fixedSize().layoutProbe("review-action")')
replace('''                Button("Done") { model.dismissCollection(review: actionToken ?? reviewOwner) }.pointingCursor().keyboardShortcut(.cancelAction).fixedSize()
            }
            List {''','''                Button("Done") { model.dismissCollection(review: actionToken ?? reviewOwner) }.pointingCursor().keyboardShortcut(.cancelAction).fixedSize()
            }.layoutProbe("collection-header")
            List {''')
replace('''                            .disabled(!model.collectionReviewMatches(actionToken) || model.isRemoving || model.isValidatingCollection || model.isRemovingFromCollection(item.id))''', '''                            .layoutProbe("collection-remove:\(item.id)")
                            .disabled(!model.collectionReviewMatches(actionToken) || model.isRemoving || model.isValidatingCollection || model.isRemovingFromCollection(item.id))''')
replace('''            }.listStyle(.inset)
            HStack {''','''            }.listStyle(.inset).layoutProbe("collection-list")
            HStack {''')
replace('''            }
        }
        .padding(26).frame(width: 720, height: 470)''','''            }.layoutProbe("collection-actions")
        }
        .padding(26).frame(width: 720, height: 470).layoutProbe("collection-sheet")''')
output = Path(sys.argv[1])
output.write_text(src)
src = (project / 'Sources/OrbitDisk/FileRowView.swift').read_text()
replace('        return HStack(spacing: 11) {', '        RowEvaluationProbe.record(id: item.id, selected: selected, hovered: hovered)\n        return HStack(spacing: 11) {')
replace('.frame(maxWidth: Self.contentWidth, alignment: .leading)','.frame(maxWidth: Self.contentWidth, alignment: .leading).layoutProbe("row:\\(item.id)")')
replace('}.frame(width: 76, alignment: .trailing)','}.frame(width: 76, alignment: .trailing).layoutProbe("row-size:\\(item.id)")')
replace('.frame(width: 32, height: 36).contentShape(Rectangle())','.frame(width: 32, height: 36).layoutProbe("row-action:\\(item.id)").contentShape(Rectangle())')
replace('.frame(width: 32, height: 36)\n','.frame(width: 32, height: 36).layoutProbe("row-action:\\(item.id)")\n')
output.with_name('FileRowView.swift').write_text(src)
